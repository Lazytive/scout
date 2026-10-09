//! Hardware test: known frames → real GPU encoder → replay ring → MP4 → decode.
//! Every frame counter must come back exactly once, in order, so the encode, buffer and
//! mux path provably drops and duplicates nothing. Clip cuts must start on a keyframe.
//!
//! `every_working_encoder_round_trips_exactly` repeats this for every installed,
//! allowlisted encoder of every codec, which checks our per-family settings (keyframe
//! interval, no B-frames, IDR keyframes) on the hardware at hand.
//!
//! Needs a working allowlisted H.264 encoder and ffmpeg on PATH. Run via `cargo xtask e2e`.

// Test code: panicking helpers are allowed (AGENTS.md rule 3 applies outside tests).
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use klip_buffer::RingConfig;
use klip_capture::klip_probe::{EncoderChoice, EncoderFamily};
use klip_capture::{CaptureSession, CaptureStats, EncodeConfig};
use klip_media::{Codec, NANOS_PER_SEC};
use klip_recorder::{RingSink, save_from};
use klip_testkit::{barcode, continuity, oracle};

const W: usize = 640;
const H: usize = 360;
const FPS: u64 = 60;
const FRAMES: u32 = 600;

fn vendors() -> Vec<klip_platform::GpuVendor> {
    klip_platform::list_gpus()
        .unwrap()
        .into_iter()
        .filter(|g| !g.software)
        .map(|g| g.vendor)
        .collect()
}

#[test]
#[ignore = "hardware: needs a GPU/OS H.264 encoder and ffmpeg; run via `cargo xtask e2e`"]
fn synthetic_frames_survive_encode_ring_and_mux_exactly() {
    klip_capture::klip_probe::init().unwrap();
    let encoder =
        klip_capture::klip_probe::best(Codec::H264, &vendors(), None, Duration::from_secs(10))
            .unwrap();
    round_trip(encoder, FRAMES);
}

#[test]
#[ignore = "hardware: needs GPU/OS encoders and ffmpeg; run via `cargo xtask e2e`"]
fn every_working_encoder_round_trips_exactly() {
    klip_capture::klip_probe::init().unwrap();
    let mut all: Vec<EncoderChoice> = [Codec::H264, Codec::H265, Codec::Av1]
        .into_iter()
        .flat_map(|c| klip_capture::klip_probe::candidates(c, &vendors(), None))
        .collect();
    // Media Foundation last: a session on NVIDIA's MFT breaks later NVENC sessions in the
    // same process (ADR 0009); the recorder avoids that order by ranking.
    all.sort_by_key(|c| u8::from(c.family == EncoderFamily::MediaFoundation));
    // `KLIP_TEST_ENCODERS=a,b` limits the run to those elements (diagnosis).
    if let Ok(only) = std::env::var("KLIP_TEST_ENCODERS") {
        all.retain(|c| only.split(',').any(|o| o == c.element));
    }
    let mut tested = Vec::new();
    for c in all {
        let r = klip_capture::klip_probe::probe(&c, Duration::from_secs(10));
        if !r.ok {
            println!("skip {} (probe failed: {:?})", c.element, r.error);
            continue;
        }
        round_trip(c.clone(), 240);
        tested.push(c.element);
    }
    println!("round-tripped: {tested:?}");
    assert!(!tested.is_empty());
}

/// Encode `frames` barcoded frames (one keyframe per second) with `encoder`, save the
/// whole ring and its last 2 s, and check every counter comes back exactly once.
fn round_trip(encoder: EncoderChoice, frames: u32) {
    println!("encoder: {} ({})", encoder.element, encoder.description);
    let codec = encoder.codec;

    let pipeline = gst::Pipeline::new();
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "BGRA")
        .field("width", W as i32)
        .field("height", H as i32)
        .field("framerate", gst::Fraction::new(FPS as i32, 1))
        .build();
    let appsrc = gst_app::AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        // Not blocking: a failed pipeline stops draining the queue, and a blocked push
        // would hang the test instead of failing it.
        .block(false)
        .max_bytes((W * H * 4 * 8) as u64)
        .build();
    let upload = gst::ElementFactory::make("d3d11upload").build().unwrap();
    pipeline.add_many([appsrc.upcast_ref(), &upload]).unwrap();
    appsrc.link(&upload).unwrap();
    let cfg = EncodeConfig {
        encoder: encoder.clone(),
        bitrate_kbps: 8_000,
        keyframe_interval: FPS as u32,
    };
    let chain =
        klip_capture::build_encode_chain(&pipeline, &cfg, W as u32, H as u32, FPS as u32).unwrap();
    upload.link(&chain.input).unwrap();

    let sink = Arc::new(
        RingSink::new(RingConfig {
            max_duration: 120 * NANOS_PER_SEC,
            max_bytes: u64::MAX,
        })
        .unwrap(),
    );
    let stats = Arc::new(CaptureStats::default());
    chain.attach(0, sink.clone(), stats.clone(), klip_capture::Timing::AsIs);
    let session = CaptureSession::run(
        pipeline,
        sink.clone(),
        stats.clone(),
        (W as u32, H as u32),
        None,
    )
    .unwrap();

    let frame_ns = NANOS_PER_SEC as u64 / FPS;
    // Real time, like a capture: the queue in front of the encoder drops frames when the
    // encoder falls behind, which a flood of frames would provoke.
    let t0 = Instant::now();
    for i in 0..frames {
        let due = t0 + Duration::from_nanos(u64::from(i) * frame_ns);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let mut buf = gst::Buffer::from_mut_slice(barcode::render_bgra(i, W, H, i % 60 == 0));
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(u64::from(i) * frame_ns));
            b.set_duration(gst::ClockTime::from_nseconds(frame_ns));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while appsrc.current_level_bytes() >= appsrc.max_bytes() {
            assert_eq!(
                session
                    .stats()
                    .errors
                    .load(std::sync::atomic::Ordering::Relaxed),
                0,
                "{}: pipeline error: {:?}",
                encoder.element,
                sink.last_error()
            );
            assert!(
                Instant::now() < deadline,
                "{}: encoder stalled",
                encoder.element
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        appsrc.push_buffer(buf).unwrap();
    }
    appsrc.end_of_stream().unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    while sink.stats().packets[0] < frames as usize {
        assert!(
            Instant::now() < deadline,
            "encoder produced {} of {frames}",
            sink.stats().packets[0]
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    session.stop();
    let st = sink.stats();
    println!("ring: {st:?}");
    assert_eq!(
        stats
            .encoder_queue_drops
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "{}: frames dropped in front of the encoder",
        encoder.element
    );
    assert_eq!(st.rejected, 0);
    assert_eq!(
        st.keyframes,
        (frames / 60) as usize,
        "one keyframe per second"
    );

    let dir = tempfile::tempdir().unwrap();

    // Whole buffer: every counter exactly once.
    let all = save_from(&sink, dir.path(), 120 * NANOS_PER_SEC, &[]).unwrap();
    println!(
        "full save: {} frames, {} bytes, {:.2} ms",
        all.frames, all.bytes, all.save_ms
    );
    let counters = oracle::decode_counters(&all.path, W, H).unwrap();
    let report = continuity::analyze(&counters);
    println!("full: {report}");
    assert!(report.is_exact(), "{report}");
    assert_eq!((report.first, report.last), (Some(0), Some(frames - 1)));
    assert_eq!(report.frames, frames as usize);

    // Last 2 s: starts at the keyframe at or before (end - 2 s), ends at the last frame.
    let tail = save_from(&sink, dir.path(), 2 * NANOS_PER_SEC, &[]).unwrap();
    let counters = oracle::decode_counters(&tail.path, W, H).unwrap();
    let report = continuity::analyze(&counters);
    println!("tail: {report}");
    assert!(report.is_exact(), "{report}");
    let first = report.first.unwrap();
    assert_eq!(first % 60, 0, "clip starts on a keyframe");
    assert_eq!(report.last, Some(frames - 1));
    assert_eq!(
        report.frames, 120,
        "exactly 2 s: the keyframe sits on the cut point"
    );

    let streams = oracle::probe_streams(&tail.path).unwrap();
    let want = match codec {
        Codec::H264 => "h264",
        Codec::H265 => "hevc",
        _ => "av1",
    };
    assert_eq!(streams[0].codec_name.as_deref(), Some(want));
    assert_eq!(streams[0].avg_fps(), Some(60.0));
}
