//! Hardware test: when the first encoder can't start, the recorder moves on to the next
//! one and records with it; the attempts are reported in the status.
//!
//! Captures the primary monitor (desktop), so it needs a display and a working encoder.
//! Run via `cargo xtask e2e`.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use klip_capture::klip_probe::{self, EncoderChoice, EncoderFamily, EncoderVendor};
use klip_capture::{EncodeConfig, VideoConfig, VideoTarget};
use klip_media::{Codec, NANOS_PER_SEC};
use klip_recorder::{Recorder, RecorderConfig};

/// Linux: a screen-cast session restored from the e2e token (the CLI tests' monitor; see
/// `apps/klip-cli/tests/common`), so this runs without the desktop's dialog once that
/// token exists.
fn portal_session() -> klip_capture::PortalSession {
    let path = std::env::var_os("KLIP_E2E_PORTAL_TOKEN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let state = std::env::var_os("XDG_STATE_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/state")
                });
            let output = std::env::var("KLIP_E2E_OUTPUT").unwrap_or_else(|_| "secondary".into());
            state
                .join("klip-e2e")
                .join(format!("portal-{output}.token"))
        });
    // Same app id as the CLI: the portal keeps restore tokens per app.
    let _ = klip_platform::register_app_id(klip_platform::APP_ID);
    let token = std::fs::read_to_string(&path)
        .ok()
        .map(|t| t.trim().to_string());
    let cast = klip_platform::ScreenCast::start(&klip_platform::ScreenCastRequest {
        source: klip_platform::ScreenCastSource::Monitor,
        show_cursor: false,
        restore_token: token,
    })
    .unwrap();
    if let Some(t) = cast.restore_token() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, t).unwrap();
    }
    klip_capture::PortalSession(std::sync::Arc::new(cast))
}

#[test]
#[ignore = "hardware: needs a display and a GPU/OS H.264 encoder; run via `cargo xtask e2e`"]
fn falls_back_to_the_next_encoder() {
    klip_probe::init().unwrap();
    klip_platform::enable_dpi_awareness();
    let vendors: Vec<_> = klip_platform::list_gpus()
        .unwrap()
        .into_iter()
        .filter(|g| !g.software)
        .map(|g| g.vendor)
        .collect();
    let working = klip_probe::best(Codec::H264, &vendors, None, Duration::from_secs(10)).unwrap();
    // An allowlisted encoder that isn't installed here: building it fails.
    let missing = ["amfh264enc", "qsvh264enc", "vah264enc"]
        .into_iter()
        .find(|n| gstreamer::ElementFactory::find(n).is_none())
        .expect("one of AMF/QSV/VA-API is absent on any single machine");
    let broken = EncoderChoice {
        element: missing.into(),
        codec: Codec::H264,
        family: EncoderFamily::Amf,
        vendor: EncoderVendor::Amd,
        description: "not installed".into(),
        rank: 0,
        gpu: None,
        device_index: 0,
    };
    let (target, portal) = if cfg!(target_os = "linux") {
        (VideoTarget::Portal, Some(portal_session()))
    } else {
        (VideoTarget::PrimaryMonitor, None)
    };
    let dir = tempfile::tempdir().unwrap();
    let cfg = RecorderConfig {
        buffer: 10 * NANOS_PER_SEC,
        max_buffer_bytes: 64 * 1024 * 1024,
        clip_dir: dir.path().to_path_buf(),
        video: VideoConfig {
            target,
            fps: 30,
            max_width: 1280,
            max_height: 720,
            show_cursor: false,
            api: Default::default(),
            output: None,
            portal,
        },
        encode: EncodeConfig {
            encoder: broken,
            bitrate_kbps: 4000,
            keyframe_interval: 30,
        },
        encoder_fallbacks: vec![working.clone()],
        audio: None,
        session: None,
    };
    let t = Instant::now();
    let rec = Recorder::start(cfg).unwrap();
    println!("started in {} ms", t.elapsed().as_millis());
    let attempts = rec.encoder_attempts();
    println!("{attempts:?}");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].element, missing);
    assert!(attempts[0].error.is_some());
    assert_eq!(attempts[1].element, working.element);
    assert!(attempts[1].error.is_none());
    let status = rec.status();
    assert_eq!(status["encoder"], working.element.as_str());
    assert!(status["last_error"].is_null(), "{status}");
    assert!(status["capture_packets"].as_u64().unwrap() > 0);
    rec.stop_capture();
}
