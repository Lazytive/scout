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

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{H, Harness, W, vendors};
use klip_buffer::RingConfig;
use klip_capture::klip_probe::{EncoderChoice, EncoderFamily};
use klip_media::{Codec, NANOS_PER_SEC};
use klip_recorder::{RingSink, save_from};
use klip_testkit::{continuity, oracle};

const FRAMES: u32 = 600;

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
    let codec = encoder.codec;
    let sink = Arc::new(
        RingSink::new(RingConfig {
            max_duration: 120 * NANOS_PER_SEC,
            max_bytes: u64::MAX,
        })
        .unwrap(),
    );
    let h = Harness::start(encoder, sink.clone());
    for i in 0..frames {
        h.push(i);
    }
    h.finish(frames);
    let st = sink.stats();
    println!("ring: {st:?}");
    assert_eq!(st.rejected, 0);
    assert_eq!(
        st.keyframes,
        (frames / 60) as usize,
        "one keyframe per second"
    );

    let dir = tempfile::tempdir().unwrap();

    // Whole buffer: every counter exactly once.
    let all = save_from(&sink, dir.path(), 120 * NANOS_PER_SEC, &[], &[]).unwrap();
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

    // A thumbnail of the end of the clip (overlay pop-up, library).
    let t = std::time::Instant::now();
    let jpeg = klip_capture::thumbnail::jpeg_near_end(
        &all.path,
        std::time::Duration::from_millis(500),
        320,
    )
    .unwrap();
    println!("thumbnail: {} bytes in {:?}", jpeg.len(), t.elapsed());
    assert!(jpeg.starts_with(&[0xFF, 0xD8, 0xFF]) && jpeg.ends_with(&[0xFF, 0xD9]));
    let thumb = dir.path().join("thumb.jpg");
    std::fs::write(&thumb, &jpeg).unwrap();
    let s = oracle::probe_streams(&thumb).unwrap();
    assert_eq!(s[0].width, Some(320), "{s:?}");

    // Last 2 s: starts at the keyframe at or before (end - 2 s), ends at the last frame.
    let tail = save_from(&sink, dir.path(), 2 * NANOS_PER_SEC, &[], &[]).unwrap();
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
