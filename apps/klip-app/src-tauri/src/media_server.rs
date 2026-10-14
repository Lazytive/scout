//! Linux: the preview's video, sound and pictures over loopback HTTP. WebKitGTK plays
//! `<video>` and `<audio>` only from http(s), file and blob URLs: its GStreamer source
//! has no element for a custom scheme such as Tauri's `asset:`, so a clip from the asset
//! protocol fails ("FormatError") before a byte is read (found on Bazzite, 2026-10-14,
//! WebKitGTK 2.54). On Linux the page's file URLs point here instead (`ipc::media_base`,
//! `fileUrl` in `api.ts`); WebView2 plays the asset protocol, so Windows keeps that.
//!
//! It serves exactly what the asset protocol may: files in its scope (the ones the engine
//! names for the preview, `ipc::editor_media`, and the ones the user picked in a dialog).
//! URLs are `http://127.0.0.1:<port>/<token>/<percent-encoded absolute path>`. The token
//! (128 random bits) keeps other local programs out; a wrong token, a path outside the
//! scope and a missing file all get the same 404. Loopback only; GET and HEAD; one byte
//! range; one request per connection; bounded connections, header size and timeouts.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Connections served at once; more are closed at once (the preview opens a few).
const MAX_CONNECTIONS: usize = 32;
/// Request line and headers together.
const MAX_HEAD: usize = 16 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// The app page's origins (Linux, Windows) whose requests may read the responses (CORS:
/// the editor's `<video>` is `crossorigin="anonymous"`, the sound peaks use `fetch`).
const APP_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// Whether a file may be served (the asset protocol's scope).
pub type Allowed = dyn Fn(&Path) -> bool + Send + Sync;

pub struct MediaServer {
    base: String,
}

impl MediaServer {
    /// Listen on a free loopback port; each connection is served on its own thread.
    pub fn start(allowed: Arc<Allowed>) -> std::io::Result<MediaServer> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|e| std::io::Error::other(e.to_string()))?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let base = format!("http://127.0.0.1:{port}/{token}/");
        std::thread::Builder::new()
            .name("klip-media-server".into())
            .spawn(move || accept(&listener, &token, &allowed))?;
        Ok(MediaServer { base })
    }

    /// `http://127.0.0.1:<port>/<token>/`: a file's URL is this plus its percent-encoded
    /// absolute path.
    pub fn base(&self) -> &str {
        &self.base
    }
}

fn accept(listener: &TcpListener, token: &str, allowed: &Arc<Allowed>) {
    let active = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let (token, allowed, done) = (token.to_string(), allowed.clone(), active.clone());
        let spawned = std::thread::Builder::new()
            .name("klip-media-conn".into())
            .spawn(move || {
                crate::guard::guarded("media server", || {
                    if let Err(e) = serve(stream, &token, allowed.as_ref()) {
                        tracing::debug!(error = %e, "media server connection");
                    }
                });
                done.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

fn serve(stream: TcpStream, token: &str, allowed: &Allowed) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some(head) = read_head(&mut reader)? else {
        return Ok(());
    };
    let reply = answer(&head, token, allowed);
    let mut out = std::io::BufWriter::new(stream);
    out.write_all(reply.head().as_bytes())?;
    if let Some((path, start, len)) = &reply.body {
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(*start))?;
        std::io::copy(&mut file.take(*len), &mut out)?;
    }
    out.flush()
}

/// The request line and headers, without the blank line; `None` at end of input or when
/// they don't fit in [`MAX_HEAD`].
fn read_head(r: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut head = String::new();
    loop {
        let room = u64::try_from(MAX_HEAD.saturating_sub(head.len())).unwrap_or(0);
        let mut line = String::new();
        if r.by_ref().take(room).read_line(&mut line)? == 0 || !line.ends_with('\n') {
            return Ok(None);
        }
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            return Ok(Some(head));
        }
        head.push_str(&line);
    }
}

/// A response: status, headers, and the file range to send after them.
#[derive(Debug, PartialEq, Eq)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    /// (file, first byte, length); none for HEAD, errors and empty files.
    body: Option<(PathBuf, u64, u64)>,
}

impl Reply {
    fn status(status: u16) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Length", "0".into())],
            body: None,
        }
    }

    fn head(&self) -> String {
        let reason = match self.status {
            200 => "OK",
            206 => "Partial Content",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            416 => "Range Not Satisfiable",
            _ => "Error",
        };
        let mut s = format!("HTTP/1.1 {} {reason}\r\n", self.status);
        for (k, v) in &self.headers {
            s.push_str(&format!("{k}: {v}\r\n"));
        }
        s.push_str("Cache-Control: no-store\r\nConnection: close\r\n\r\n");
        s
    }
}

/// What to answer to the request `head`.
fn answer(head: &str, token: &str, allowed: &Allowed) -> Reply {
    let mut lines = head.lines();
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (parts.next().unwrap_or_default(), parts.next());
    let Some(target) = target else {
        return Reply::status(400);
    };
    let (mut range, mut origin) = (None, None);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.trim().eq_ignore_ascii_case("range") {
                range = Some(value);
            } else if name.trim().eq_ignore_ascii_case("origin") {
                origin = Some(value);
            }
        }
    }
    let mut reply = match (method, file_of(target, token)) {
        ("GET" | "HEAD", Some(path)) if allowed(&path) => file_reply(path, range),
        ("GET" | "HEAD", _) => Reply::status(404),
        _ => Reply::status(405),
    };
    if method == "HEAD" {
        reply.body = None;
    }
    if let Some(o) = origin.filter(|o| APP_ORIGINS.contains(o)) {
        reply
            .headers
            .push(("Access-Control-Allow-Origin", o.to_string()));
        reply.headers.push((
            "Access-Control-Expose-Headers",
            "Content-Range, Content-Length, Accept-Ranges".into(),
        ));
        reply.headers.push(("Vary", "Origin".into()));
    }
    reply
}

/// The absolute path a request target names, if it carries the token.
fn file_of(target: &str, token: &str) -> Option<PathBuf> {
    let rest = target.strip_prefix('/')?;
    let (tok, encoded) = rest.split_once('/')?;
    if !same(tok.as_bytes(), token.as_bytes()) {
        return None;
    }
    let encoded = encoded.split(['?', '#']).next().unwrap_or_default();
    let path = PathBuf::from(percent_decode(encoded)?);
    (path.is_absolute() && !path.as_os_str().is_empty()).then_some(path)
}

/// Equal, in time that doesn't depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn percent_decode(s: &str) -> Option<String> {
    let hex = |b: u8| {
        char::from(b)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = hex(bytes.next()?)?;
            let lo = hex(bytes.next()?)?;
            out.push(hi.checked_mul(16)?.checked_add(lo)?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok().filter(|s| !s.contains('\0'))
}

fn file_reply(path: PathBuf, range: Option<&str>) -> Reply {
    let Some(len) = std::fs::metadata(&path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
    else {
        return Reply::status(404);
    };
    let mut reply = Reply {
        status: 200,
        headers: vec![
            ("Content-Type", content_type(&path).into()),
            ("Accept-Ranges", "bytes".into()),
        ],
        body: None,
    };
    let (start, count) = match byte_range(range, len) {
        Span::Whole => (0, len),
        Span::Part(first, last) => {
            reply.status = 206;
            reply
                .headers
                .push(("Content-Range", format!("bytes {first}-{last}/{len}")));
            (first, last.saturating_sub(first).saturating_add(1))
        }
        Span::Unsatisfiable => {
            let mut r = Reply::status(416);
            r.headers.push(("Content-Range", format!("bytes */{len}")));
            return r;
        }
    };
    reply.headers.push(("Content-Length", count.to_string()));
    if count > 0 {
        reply.body = Some((path, start, count));
    }
    reply
}

#[derive(Debug, PartialEq, Eq)]
enum Span {
    Whole,
    /// First and last byte, both included.
    Part(u64, u64),
    Unsatisfiable,
}

/// A `Range` header against a file of `len` bytes. One range (`bytes=a-b`, `bytes=a-`,
/// `bytes=-n`); several ranges, or a header that doesn't parse, get the whole file.
fn byte_range(header: Option<&str>, len: u64) -> Span {
    let Some(spec) = header
        .and_then(|h| h.trim().strip_prefix("bytes="))
        .filter(|s| !s.contains(','))
    else {
        return Span::Whole;
    };
    let Some((a, b)) = spec.split_once('-') else {
        return Span::Whole;
    };
    let (a, b) = (a.trim(), b.trim());
    let last = len.checked_sub(1);
    if a.is_empty() {
        // The last `n` bytes.
        return match (b.parse::<u64>(), last) {
            (Ok(n), Some(last)) if n > 0 => Span::Part(len.saturating_sub(n), last),
            (Ok(_), _) => Span::Unsatisfiable,
            (Err(_), _) => Span::Whole,
        };
    }
    let Ok(first) = a.parse::<u64>() else {
        return Span::Whole;
    };
    let end = match b {
        "" => None,
        b => match b.parse::<u64>() {
            Ok(e) if e >= first => Some(e),
            _ => return Span::Whole,
        },
    };
    match last {
        Some(last) if first <= last => Span::Part(first, end.map_or(last, |e| e.min(last))),
        _ => Span::Unsatisfiable,
    }
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "m4a" | "aac" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "00112233445566778899aabbccddeeff";

    /// As the page's `encodeURIComponent`.
    fn encode(path: &Path) -> String {
        let mut s = String::new();
        for b in path.to_string_lossy().bytes() {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                s.push(char::from(b));
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
        s
    }

    fn url_path(path: &Path) -> String {
        format!("/{TOKEN}/{}", encode(path))
    }

    #[test]
    fn byte_ranges() {
        assert_eq!(byte_range(None, 10), Span::Whole);
        assert_eq!(byte_range(Some("bytes=0-"), 10), Span::Part(0, 9));
        assert_eq!(byte_range(Some("bytes=2-4"), 10), Span::Part(2, 4));
        assert_eq!(byte_range(Some("bytes=2-400"), 10), Span::Part(2, 9));
        assert_eq!(byte_range(Some("bytes=-3"), 10), Span::Part(7, 9));
        assert_eq!(byte_range(Some("bytes=-30"), 10), Span::Part(0, 9));
        assert_eq!(byte_range(Some("bytes=10-"), 10), Span::Unsatisfiable);
        assert_eq!(byte_range(Some("bytes=0-"), 0), Span::Unsatisfiable);
        assert_eq!(byte_range(Some("bytes=-0"), 10), Span::Unsatisfiable);
        for odd in [
            "bytes=4-2",
            "bytes=0-1,4-5",
            "items=0-1",
            "bytes=x-",
            "bytes=1-y",
            "bytes=-",
        ] {
            assert_eq!(byte_range(Some(odd), 10), Span::Whole, "{odd}");
        }
    }

    #[test]
    fn only_allowed_files_with_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("a clip é.mp4");
        std::fs::write(&clip, b"0123456789").unwrap();
        let other = dir.path().join("secret.txt");
        std::fs::write(&other, b"no").unwrap();
        let allowed_clip = clip.clone();
        let allowed = move |p: &Path| p == allowed_clip;
        let get = |target: &str, extra: &str| {
            answer(
                &format!("GET {target} HTTP/1.1\r\nHost: x\r\n{extra}"),
                TOKEN,
                &allowed,
            )
        };

        let whole = get(&url_path(&clip), "");
        assert_eq!(whole.status, 200);
        assert_eq!(whole.body, Some((clip.clone(), 0, 10)));
        assert!(
            whole
                .headers
                .contains(&("Content-Type", "video/mp4".into()))
        );
        assert!(whole.headers.contains(&("Content-Length", "10".into())));

        let part = get(
            &url_path(&clip),
            "Range: bytes=4-\r\nOrigin: tauri://localhost\r\n",
        );
        assert_eq!(part.status, 206);
        assert_eq!(part.body, Some((clip.clone(), 4, 6)));
        assert!(
            part.headers
                .contains(&("Content-Range", "bytes 4-9/10".into()))
        );
        assert!(
            part.headers
                .contains(&("Access-Control-Allow-Origin", "tauri://localhost".into()))
        );

        // Not the app's page: no CORS header, so a web page can't read it.
        let foreign = get(&url_path(&clip), "Origin: https://example.com\r\n");
        assert!(
            !foreign
                .headers
                .iter()
                .any(|(k, _)| *k == "Access-Control-Allow-Origin")
        );

        assert_eq!(get(&url_path(&clip), "range: bytes=20-\r\n").status, 416);
        assert_eq!(get(&url_path(&other), "").status, 404, "outside the scope");
        let wrong = url_path(&clip).replacen(TOKEN, "ffffffffffffffffffffffffffffffff", 1);
        assert_eq!(get(&wrong, "").status, 404, "wrong token");
        assert_eq!(get(&format!("/{TOKEN}/relative.mp4"), "").status, 404);
        assert_eq!(get(&format!("/{TOKEN}/%2Fx%00.mp4"), "").status, 404, "NUL");
        assert_eq!(get(&format!("/{TOKEN}/%ZZ"), "").status, 404, "bad escape");
        assert_eq!(get("/", "").status, 404);

        let head = answer(
            &format!("HEAD {} HTTP/1.1\r\n", url_path(&clip)),
            TOKEN,
            &allowed,
        );
        assert_eq!((head.status, head.body), (200, None));
        let post = answer(
            &format!("POST {} HTTP/1.1\r\n", url_path(&clip)),
            TOKEN,
            &allowed,
        );
        assert_eq!(post.status, 405);
        assert_eq!(answer("GARBAGE", TOKEN, &allowed).status, 400);
    }

    /// Over a real socket: a range comes back byte-exact, and a stranger without the token
    /// gets nothing.
    #[test]
    fn serves_ranges_over_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("c.mp4");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&clip, &data).unwrap();
        let allowed_clip = clip.clone();
        let server = MediaServer::start(Arc::new(move |p: &Path| p == allowed_clip)).unwrap();
        let base = server.base().to_string();
        assert!(base.starts_with("http://127.0.0.1:") && base.ends_with('/'));
        let rest = base.trim_start_matches("http://");
        let (addr, token) = rest.trim_end_matches('/').split_once('/').unwrap();
        assert_eq!(token.len(), 32);

        let fetch = |target: &str, range: &str| -> (String, Vec<u8>) {
            let mut s = TcpStream::connect(addr).unwrap();
            write!(s, "GET {target} HTTP/1.1\r\nHost: {addr}\r\n{range}\r\n").unwrap();
            let mut all = Vec::new();
            s.read_to_end(&mut all).unwrap();
            let split = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let head = String::from_utf8(all[..split].to_vec()).unwrap();
            (head, all[split + 4..].to_vec())
        };
        let target = format!("/{token}/{}", encode(&clip));
        let (head, body) = fetch(&target, "Range: bytes=100000-100999\r\n");
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert_eq!(body, data[100_000..101_000]);
        let (head, body) = fetch(&target, "");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body, data);
        let (head, body) = fetch(&format!("/nope/{}", encode(&clip)), "");
        assert!(head.starts_with("HTTP/1.1 404"), "{head}");
        assert!(body.is_empty());
    }
}
