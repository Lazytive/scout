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
    let dir = tempfile::tempdir().unwrap();
    let cfg = RecorderConfig {
        buffer: 10 * NANOS_PER_SEC,
        max_buffer_bytes: 64 * 1024 * 1024,
        clip_dir: dir.path().to_path_buf(),
        video: VideoConfig {
            target: VideoTarget::PrimaryMonitor,
            fps: 30,
            max_width: 1280,
            max_height: 720,
            show_cursor: false,
            api: Default::default(),
            output: None,
            portal: None,
        },
        encode: EncodeConfig {
            encoder: broken,
            bitrate_kbps: 4000,
            keyframe_interval: 30,
        },
        encoder_fallbacks: vec![working.clone()],
        audio: None,
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
