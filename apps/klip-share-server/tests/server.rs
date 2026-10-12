//! The share server over real HTTP on 127.0.0.1 (M11.6): the whole life of a clip, then
//! hostile and broken requests (raw TCP where a client library would be too polite).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use klip_media::{Codec, NANOS_PER_SEC, Packet, TrackInfo, TrackParams, VideoParams};
use klip_mp4::{Mp4Options, Mp4Track};
use klip_share::{ServerInfo, Uploaded};
use klip_share_server::config::Config;
use klip_share_server::store::Store;
use klip_share_server::{AppState, serve};

struct Server {
    addr: SocketAddr,
    base: String,
    token: String,
    store: std::sync::Arc<Store>,
    data: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
    _stop: tokio::sync::oneshot::Sender<()>,
}

fn start(tweak: impl FnOnce(&mut Config)) -> Server {
    let data = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let mut cfg = Config {
        data_dir: data.path().to_path_buf(),
        public_url: format!("http://{addr}"),
        ..Config::default()
    };
    tweak(&mut cfg);
    cfg.check().unwrap();
    cfg.check_public_url().unwrap();
    let state = AppState::new(cfg).unwrap();
    let token = state.store.add_token("test").unwrap();
    let store = state.store.clone();
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    rt.spawn(serve(listener, state, async {
        let _ = rx.await;
    }));
    Server {
        addr,
        base: format!("http://{addr}"),
        token,
        store,
        data,
        _rt: rt,
        _stop: stop,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

/// A 60 fps MP4 of made-up H.264 packets (the server only reads its boxes).
fn mp4(dir: &Path, frames: usize) -> Vec<u8> {
    let path = dir.join(format!("made-{frames}.mp4"));
    let info = TrackInfo {
        codec: Codec::H264,
        codec_data: Bytes::from_static(&[1, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 0]),
        params: TrackParams::Video(VideoParams {
            width: 1280,
            height: 720,
            colorimetry: None,
        }),
        bitrate: None,
    };
    let frame = NANOS_PER_SEC / 60;
    let packets: Vec<Packet> = (0..frames)
        .map(|i| Packet {
            pts: i as i64 * frame,
            dts: i as i64 * frame,
            duration: frame,
            keyframe: i % 60 == 0,
            data: Bytes::from(vec![7u8; 64]),
        })
        .collect();
    klip_mp4::save_mp4_atomic(
        &path,
        &[Mp4Track::new(&info, &packets)],
        &Mp4Options::default(),
    )
    .unwrap();
    std::fs::read(&path).unwrap()
}

fn upload(s: &Server, token: &str, query: &str, body: &[u8]) -> (u16, String) {
    let mut res = agent()
        .post(&format!("{}/api/v1/clips{query}", s.base))
        .header("Authorization", &format!("Bearer {token}"))
        .send(body)
        .unwrap();
    (
        res.status().as_u16(),
        res.body_mut().read_to_string().unwrap(),
    )
}

/// Send `head` (and `body`), wait `pause`, return the start of the answer.
fn raw(addr: SocketAddr, head: &str, body: &[u8], pause: Duration) -> String {
    let mut c = TcpStream::connect(addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let _ = c.write_all(head.as_bytes());
    let _ = c.write_all(body);
    std::thread::sleep(pause);
    let mut buf = vec![0u8; 4096];
    let n = c.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn tmp_is_empty(s: &Server) -> bool {
    std::fs::read_dir(s.data.path().join("tmp"))
        .unwrap()
        .count()
        == 0
}

#[test]
fn a_clip_is_uploaded_shown_streamed_given_a_poster_and_deleted() {
    let s = start(|_| {});
    let work = tempfile::tempdir().unwrap();
    let video = mp4(work.path(), 120);

    // Info needs the token.
    let mut r = agent()
        .get(&format!("{}/api/v1/info", s.base))
        .header("Authorization", &format!("Bearer {}", s.token))
        .call()
        .unwrap();
    assert_eq!(r.status(), 200);
    let info: ServerInfo = serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap();
    assert_eq!(
        (info.server.as_str(), info.token_name.as_str()),
        ("scout-share", "test")
    );
    assert_eq!(info.max_upload_bytes, 500_000_000);
    let r = agent()
        .get(&format!("{}/api/v1/info", s.base))
        .call()
        .unwrap();
    assert_eq!(r.status(), 401);

    let (status, body) = upload(&s, &s.token, "?title=Ace%20%3Cb%3Eround%3C%2Fb%3E", &video);
    assert_eq!(status, 201, "{body}");
    let up: Uploaded = serde_json::from_str(&body).unwrap();
    assert!(klip_share::valid_id(&up.id));
    assert_eq!(up.url, format!("{}/c/{}", s.base, up.id));
    assert_eq!(up.video_url, format!("{}.mp4", up.url));
    assert_eq!(up.bytes, video.len() as u64);
    assert_eq!(up.expires_unix, 0);
    assert!(up.manage_token.starts_with("ssm_"));
    assert!(tmp_is_empty(&s));
    let meta = s.store.read_meta(&up.id).unwrap();
    assert_eq!(
        (meta.width, meta.height, meta.codec.as_str()),
        (1280, 720, "H.264")
    );
    assert_eq!(meta.duration_ms, 2000);
    assert_eq!(s.store.used(), video.len() as u64);
    assert!(
        !std::fs::read_to_string(s.data.path().join("clips").join(&up.id).join("meta.json"))
            .unwrap()
            .contains(&up.manage_token)
    );

    // The page: Open Graph tags, the title escaped, no indexing.
    let mut r = agent().get(&up.url).call().unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["x-robots-tag"], "noindex, nofollow");
    assert!(
        r.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    let html = r.body_mut().read_to_string().unwrap();
    assert!(html.contains(&format!(
        "<meta property=\"og:video\" content=\"{}\">",
        up.video_url
    )));
    assert!(html.contains("og:video:width\" content=\"1280\""));
    assert!(html.contains("Ace &lt;b&gt;round&lt;/b&gt;"), "{html}");
    assert!(!html.contains("og:image"));

    // The video, whole and by range.
    let mut r = agent().get(&up.video_url).call().unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "video/mp4");
    assert_eq!(r.body_mut().read_to_vec().unwrap(), video);
    let mut r = agent()
        .get(&up.video_url)
        .header("Range", "bytes=0-9")
        .call()
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(
        r.headers()["content-range"],
        format!("bytes 0-9/{}", video.len()).as_str()
    );
    assert_eq!(r.body_mut().read_to_vec().unwrap(), video[..10]);

    // The poster: only with the manage token, only a picture.
    let poster_url = format!("{}/api/v1/clips/{}/poster", s.base, up.id);
    let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
    for (token, body, want) in [
        (s.token.as_str(), &jpeg[..], 401),
        (up.manage_token.as_str(), b"<svg onload=x>".as_slice(), 415),
        (up.manage_token.as_str(), &jpeg[..], 204),
    ] {
        let r = agent()
            .put(&poster_url)
            .header("Authorization", &format!("Bearer {token}"))
            .send(body)
            .unwrap();
        assert_eq!(r.status(), want);
    }
    let mut r = agent().get(&format!("{}.jpg", up.url)).call().unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/jpeg");
    assert_eq!(r.body_mut().read_to_vec().unwrap(), jpeg);
    assert_eq!(
        agent()
            .get(&format!("{}.png", up.url))
            .call()
            .unwrap()
            .status(),
        404
    );
    let html = agent()
        .get(&up.url)
        .call()
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
    assert!(html.contains(&format!("og:image\" content=\"{}.jpg\"", up.url)));

    // Delete: the manage token only, then everything is gone.
    let del = format!("{}/api/v1/clips/{}", s.base, up.id);
    let r = agent()
        .delete(&del)
        .header("Authorization", &format!("Bearer {}", s.token))
        .call()
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = agent()
        .delete(&del)
        .header("Authorization", &format!("Bearer {}", up.manage_token))
        .call()
        .unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(agent().get(&up.url).call().unwrap().status(), 404);
    assert_eq!(agent().get(&up.video_url).call().unwrap().status(), 404);
    let r = agent()
        .delete(&del)
        .header("Authorization", &format!("Bearer {}", up.manage_token))
        .call()
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(s.store.used(), 0);

    // The rest of the site says nothing.
    let mut r = agent()
        .get(&format!("{}/robots.txt", s.base))
        .call()
        .unwrap();
    assert!(
        r.body_mut()
            .read_to_string()
            .unwrap()
            .contains("Disallow: /")
    );
    assert_eq!(
        agent()
            .get(&format!("{}/healthz", s.base))
            .call()
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        agent()
            .get(&format!("{}/c/", s.base))
            .call()
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        agent()
            .get(&format!("{}/tokens.json", s.base))
            .call()
            .unwrap()
            .status(),
        404
    );
}

#[test]
fn hostile_and_broken_uploads_are_refused_and_leave_nothing() {
    let s = start(|c| {
        c.max_upload_mb = 1;
        c.quota_gb = 1;
        c.max_expiry_days = 7;
        c.default_expiry_days = 7;
        c.upload_idle_secs = 1;
        c.max_concurrent_uploads = 1;
    });
    let work = tempfile::tempdir().unwrap();
    let video = mp4(work.path(), 60);
    let zeros = format!("sst_{}", "0".repeat(64));

    assert_eq!(upload(&s, "", "", &video).0, 401);
    assert_eq!(upload(&s, &zeros, "", &video).0, 401);
    let (status, body) = upload(&s, &s.token, "", b"hello, not a video at all");
    assert_eq!((status, body.contains("not_mp4")), (415, true), "{body}");
    for q in ["?expires_days=0", "?expires_days=8", "?expires_days=soon"] {
        assert_eq!(upload(&s, &s.token, q, &video).0, 400, "{q}");
    }
    let (status, body) = upload(&s, &s.token, "?expires_days=3", &video);
    assert_eq!(status, 201, "{body}");
    let up: Uploaded = serde_json::from_str(&body).unwrap();
    let now = klip_share_server::store::now_unix();
    assert!(
        (now + 3 * 86_400).abs_diff(up.expires_unix) < 60,
        "{}",
        up.expires_unix
    );
    assert!(tmp_is_empty(&s));

    // Declared too big: refused before the body is read.
    let head = format!(
        "POST /api/v1/clips HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: 1000001\r\n\r\n",
        s.token
    );
    let r = raw(s.addr, &head, b"", Duration::from_millis(300));
    assert!(r.starts_with("HTTP/1.1 413"), "{r}");
    // Chunked, no length: stopped once it passes the limit.
    let head = format!(
        "POST /api/v1/clips HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\n\r\n",
        s.token
    );
    let mut chunks = Vec::new();
    for _ in 0..20 {
        chunks.extend_from_slice(format!("{:x}\r\n", 64 * 1024).as_bytes());
        chunks.extend_from_slice(&vec![1u8; 64 * 1024]);
        chunks.extend_from_slice(b"\r\n");
    }
    let r = raw(s.addr, &head, &chunks, Duration::from_millis(300));
    assert!(r.starts_with("HTTP/1.1 413"), "{r}");
    // Stalls: dropped after upload_idle_secs; while one runs the next gets 429.
    let head = format!(
        "POST /api/v1/clips HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: 1000\r\n\r\n",
        s.token
    );
    let stalled = {
        let addr = s.addr;
        let head = head.clone();
        std::thread::spawn(move || raw(addr, &head, b"0123456789", Duration::from_millis(2500)))
    };
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(upload(&s, &s.token, "", &video).0, 429);
    let r = stalled.join().unwrap();
    assert!(r.starts_with("HTTP/1.1 408"), "{r}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(tmp_is_empty(&s));

    // No room: the quota is checked before anything is kept.
    let quota = 1_000_000_000;
    let used = s.store.used();
    assert!(s.store.reserve(quota - used - 10, quota));
    let (status, body) = upload(&s, &s.token, "", &video);
    assert_eq!(status, 507, "{body}");
    s.store.release(quota - used - 10);
    assert_eq!(s.store.used(), used);

    // Ids that aren't ids, and a link whose time is up.
    for path in [
        "/c/..%2Ftokens.json",
        "/c/ZZZ",
        "/c/0123456789abcdef0123456789abcdef",
        "/c/0123456789ABCDEF0123456789ABCDEF.mp4",
    ] {
        let r = raw(
            s.addr,
            &format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n"),
            b"",
            Duration::ZERO,
        );
        assert!(r.starts_with("HTTP/1.1 404"), "{path}: {r}");
    }
    let meta_path = s.data.path().join("clips").join(&up.id).join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    meta["expires_unix"] = serde_json::json!(1);
    std::fs::write(&meta_path, meta.to_string()).unwrap();
    assert_eq!(agent().get(&up.url).call().unwrap().status(), 404);
    assert_eq!(s.store.sweep(now), 1);
    assert!(!meta_path.exists());
}
