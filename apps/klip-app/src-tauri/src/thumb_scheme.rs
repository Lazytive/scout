//! The `klipthumb` URI scheme: library thumbnails for the window's `<img>` tags
//! (ADR 0015). `klipthumb://localhost/<poster|strip>/<key, URI-encoded>?v=<version>`
//! (on Windows the webview spells it `http://klipthumb.localhost/…`). It answers only for
//! cached thumbnails of clips in the library (the engine's `library.thumb` checks the
//! key), never for other files, and caps what it reads.

use std::io::Read;
use std::sync::Arc;

use klip_control::{Handler, commands};
use serde_json::json;
use tauri::http::{Response, StatusCode, header};

/// Scheme name (also in the CSP's `img-src`).
pub const SCHEME: &str = "klipthumb";
/// Largest file served (the engine refuses bigger ones too).
const MAX_BYTES: u64 = 8 << 20;

/// Decode `%XX` escapes; `None` for a broken escape or text that isn't UTF-8.
pub fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = bytes.get(i.checked_add(1)?..i.checked_add(3)?)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i = i.checked_add(3)?;
        } else {
            out.push(b);
            i = i.checked_add(1)?;
        }
    }
    String::from_utf8(out).ok()
}

/// `(kind, key)` from a request path like `/strip/Apex%2FKlip_1.mp4`.
pub fn parse_path(path: &str) -> Option<(&'static str, String)> {
    let rest = path.strip_prefix('/')?;
    let (kind, key) = rest.split_once('/')?;
    let kind = match kind {
        "poster" => "poster",
        "strip" => "strip",
        _ => return None,
    };
    let key = percent_decode(key)?;
    (!key.is_empty()).then_some((kind, key))
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    let mut r = Response::new(Vec::new());
    *r.status_mut() = code;
    r
}

/// The response for a request path.
pub fn respond(handler: &Arc<dyn Handler>, path: &str) -> Response<Vec<u8>> {
    let Some((kind, key)) = parse_path(path) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let file = handler
        .handle(
            commands::LIBRARY_THUMB,
            &json!({ "key": key, "kind": kind }),
        )
        .ok()
        .and_then(|v| v.get("path")?.as_str().map(str::to_string));
    let Some(file) = file else {
        return status(StatusCode::NOT_FOUND);
    };
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&file).and_then(|f| f.take(MAX_BYTES).read_to_end(&mut bytes));
    if read.is_err() || bytes.is_empty() {
        return status(StatusCode::NOT_FOUND);
    }
    let mut r = Response::new(bytes);
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("image/jpeg"),
    );
    // The URL carries the file's version (`?v=`), so a cached copy is never stale.
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("max-age=31536000, immutable"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_escapes() {
        assert_eq!(
            parse_path("/strip/Apex%2FKlip%201.mp4"),
            Some(("strip", "Apex/Klip 1.mp4".into()))
        );
        assert_eq!(
            parse_path("/poster/%E6%97%A5.mp4"),
            Some(("poster", "日.mp4".into()))
        );
        for bad in [
            "",
            "/",
            "/poster",
            "/poster/",
            "/video/a.mp4",
            "/poster/%zz",
            "/poster/%4",
            "/poster/%FF",
        ] {
            assert_eq!(parse_path(bad), None, "{bad}");
        }
        // `..` is decoded as text; the engine refuses keys that aren't clean paths.
        assert_eq!(
            parse_path("/poster/..%2F..%2Fx.mp4"),
            Some(("poster", "../../x.mp4".into()))
        );
    }
}
