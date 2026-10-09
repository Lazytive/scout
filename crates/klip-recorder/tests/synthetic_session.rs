//! Hardware test (M5.3): known frames → real GPU encoder → ring → full-session recording.
//!
//! - A copy of the file taken while recording (what a killed recorder leaves) decodes in
//!   ffmpeg as a continuous run of frames from the first one.
//! - Finalising that copy keeps those frames.
//! - The stopped session's file is a regular MP4 (index at the end) holding every frame
//!   exactly once, at 60 fps, with the bookmark as a chapter.
//!
//! Needs a working allowlisted H.264 encoder and ffmpeg on PATH. Run via `cargo xtask e2e`.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::sync::Arc;

use common::{FRAME_NS, H, Harness, W, best_h264};
use klip_buffer::RingConfig;
use klip_media::NANOS_PER_SEC;
use klip_recorder::{RingSink, SessionConfig};
use klip_testkit::{continuity, oracle};

const FRAMES: u32 = 600;

#[test]
#[ignore = "hardware: needs a GPU/OS H.264 encoder and ffmpeg; run via `cargo xtask e2e`"]
fn session_file_survives_a_kill_and_finalises_exactly() {
    assert!(oracle::available("ffmpeg") && oracle::available("ffprobe"));
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(
        RingSink::new(RingConfig {
            max_duration: 30 * NANOS_PER_SEC,
            max_bytes: u64::MAX,
        })
        .unwrap(),
    );
    let mut cfg = SessionConfig::new(dir.path().join("rec"));
    cfg.min_free_bytes = 0;
    // Started before the first frame: the recording begins at the first keyframe.
    let session = sink.start_session(&cfg, vec!["Video".into()]).unwrap();
    let h = Harness::start(best_h264(), sink.clone());
    let killed = dir.path().join("killed.mp4");
    for i in 0..FRAMES {
        h.push(i);
        if i == 330 {
            // What a recorder killed now would leave on disk.
            std::fs::copy(session.path(), &killed).unwrap();
        }
    }
    // Frame 150 is at 2.5 s.
    let at = session.bookmark(150 * FRAME_NS as i64).unwrap();
    assert_eq!(at, Some(150 * FRAME_NS as i64));
    h.finish(FRAMES);
    let summary = sink.stop_session(session);
    println!("summary: {summary:?}");
    assert_eq!(summary.error, None);
    assert_eq!(summary.files.len(), 1);
    let file = &summary.files[0];
    assert_eq!(file.error, None);

    // The killed copy plays: frames 0.. in order, ending on a fragment boundary or inside
    // the fragment being written.
    let counters = oracle::decode_counters(&killed, W, H).unwrap();
    let report = continuity::analyze(&counters);
    println!("killed copy: {report}");
    assert_eq!(report.first, Some(0));
    assert_eq!((report.duplicates, report.backwards), (0, 0), "{report}");
    assert!(report.steps.keys().all(|s| *s == 1), "{report}");
    assert!(
        report.unreadable <= 1,
        "at most a torn last frame: {report}"
    );
    assert!(
        report.frames >= 240,
        "four fragments by frame 330: {report}"
    );

    // Recovering it (what the next start does) keeps exactly the complete fragments.
    let fin = klip_mp4::finalize(&killed, &[]).unwrap();
    println!("recovered: {fin:?}");
    let counters = oracle::decode_counters(&killed, W, H).unwrap();
    let report = continuity::analyze(&counters);
    println!("recovered copy: {report}");
    assert!(report.is_exact(), "{report}");
    assert_eq!(report.first, Some(0));
    assert_eq!(report.frames % 60, 0, "whole fragments: {report}");

    // The finished file: a regular MP4 with every frame once.
    let boxes = oracle::top_level_boxes(&file.path).unwrap();
    println!("boxes: {boxes:?}");
    assert_eq!(boxes.first().map(String::as_str), Some("ftyp"));
    assert_eq!(boxes.last().map(String::as_str), Some("moov"));
    assert!(!boxes.iter().any(|b| b == "moof"));
    assert_eq!(boxes.iter().filter(|b| *b == "mdat").count(), 10);
    let counters = oracle::decode_counters(&file.path, W, H).unwrap();
    let report = continuity::analyze(&counters);
    println!("final: {report}");
    assert!(report.is_exact(), "{report}");
    assert_eq!((report.first, report.last), (Some(0), Some(FRAMES - 1)));
    let streams = oracle::probe_streams(&file.path).unwrap();
    assert_eq!(streams[0].codec_name.as_deref(), Some("h264"));
    assert_eq!(streams[0].avg_fps(), Some(60.0));
    assert_eq!(streams[0].frames(), Some(u64::from(FRAMES)));
    let chapters = oracle::chapters(&file.path).unwrap();
    println!("chapters: {chapters:?}");
    assert_eq!(chapters.len(), 1);
    assert!((chapters[0].0 - 2.5).abs() < 0.001);
    assert_eq!(chapters[0].1, "Bookmark 1");
}
