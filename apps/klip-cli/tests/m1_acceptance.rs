//! M1 acceptance (hardware): the test game + `klip-cli record --buffer 120s` +
//! `klip-cli clip --last 30s` → a valid faststart MP4 saved in under a second, with the
//! expected duration and frame rate, passing frame-continuity checks.
//!
//! Needs a GPU encoder, an interactive desktop and ffmpeg/ffprobe on PATH. Run with
//! `cargo xtask e2e` (builds the test game first). `KLIP_E2E_CLIP_SECS` shortens the clip
//! for quick iterations (default 30).

// Test code: panicking helpers are allowed (AGENTS.md rule 3 applies outside tests).
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{
    KillOnDrop, capture_api_args, capture_args, check_portal_stream, cli, game, run_json,
    spawn_ready, stop_recorder,
};
use klip_testkit::{continuity, oracle, sync};

#[test]
#[ignore = "hardware: needs GPU encoder, desktop session, ffmpeg; run via `cargo xtask e2e`"]
fn record_buffer_and_clip_last_30s() {
    assert!(
        oracle::available("ffprobe") && oracle::available("ffmpeg"),
        "ffmpeg/ffprobe must be on PATH"
    );
    let clip_secs: u64 = std::env::var("KLIP_E2E_CLIP_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let clips = tmp.path().join("clips");
    let frames_log = tmp.path().join("frames.csv");
    let game_fps = 120;
    let capture_fps = 60;

    let (game, ready) = spawn_ready(
        game()
            .args([
                "--background",
                "--fps",
                &game_fps.to_string(),
                "--width",
                "1280",
                "--height",
                "720",
                "--duration",
                &format!("{}s", clip_secs + 40),
            ])
            .arg("--frames-log")
            .arg(&frames_log),
        Duration::from_secs(30),
    );
    let _game = KillOnDrop(game);
    println!("game: {ready}");

    let (rec, rec_ready) = spawn_ready(
        cli(&discovery)
            .args([
                "record",
                "--buffer",
                "120s",
                "--fps",
                &capture_fps.to_string(),
            ])
            .args(capture_args(&ready))
            // The game's size (a no-op for its window; scales a monitor stream down).
            .args(["--max-size", "1280x720"])
            .args(capture_api_args())
            .arg("--out")
            .arg(&clips),
        // Long enough to answer the portal's dialog on the first Linux run.
        Duration::from_secs(120),
    );
    let mut rec = KillOnDrop(rec);
    println!("recorder: {rec_ready}");
    check_portal_stream(&rec_ready, &ready);

    std::thread::sleep(Duration::from_secs(clip_secs + 3));

    let t0 = Instant::now();
    let saved = run_json(cli(&discovery).args(["clip", "--last", &format!("{clip_secs}s")]));
    let cli_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let status = run_json(cli(&discovery).arg("status"));
    stop_recorder(&discovery, &mut rec);
    println!("clip: {saved}");
    println!("status: {status}");
    println!("clip command round trip: {cli_ms:.1} ms");

    let path = PathBuf::from(saved["path"].as_str().unwrap());
    let save_ms = saved["save_ms"].as_f64().unwrap();
    assert!(save_ms < 1000.0, "save took {save_ms} ms");

    // Container: faststart, one H.264 video stream at the window size.
    assert_eq!(
        oracle::top_level_boxes(&path).unwrap(),
        ["ftyp", "moov", "mdat"]
    );
    let streams = oracle::probe_streams(&path).unwrap();
    let videos: Vec<_> = streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("video"))
        .collect();
    assert_eq!(videos.len(), 1, "{streams:?}");
    let v = videos[0];
    assert_eq!(v.codec_name.as_deref(), Some("h264"));
    assert_eq!((v.width, v.height), (Some(1280), Some(720)));
    let dur = v.duration_secs().unwrap();
    println!(
        "duration {dur:.3} s, avg fps {:?}, frames {:?}",
        v.avg_fps(),
        v.frames()
    );
    assert!(
        dur >= clip_secs as f64 - 0.05 && dur <= clip_secs as f64 + 1.1,
        "duration {dur}"
    );

    // Timing: frame intervals from the container.
    let times = oracle::video_frame_times(&path).unwrap();
    let mut intervals: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    intervals.sort_by(f64::total_cmp);
    let pct = |p: f64| intervals[((intervals.len() - 1) as f64 * p) as usize] * 1000.0;
    println!(
        "frame interval ms: p1 {:.2} p50 {:.2} p99 {:.2} max {:.2}",
        pct(0.01),
        pct(0.5),
        pct(0.99),
        pct(1.0)
    );
    let fps = (times.len() - 1) as f64 / (times.last().unwrap() - times[0]);
    println!("measured fps {fps:.2}");

    // Content: barcodes.
    let counters = oracle::decode_counters(&path, 640, 360).unwrap();
    let report = continuity::analyze(&counters);
    println!("continuity: {report}");
    let expected_step = game_fps / capture_fps;
    assert_eq!(report.unreadable, 0, "unreadable frames");
    assert_eq!(report.backwards, 0, "frames out of order");
    assert_eq!(
        report.frames,
        times.len(),
        "decoded frame count matches container"
    );
    // Frame continuity: every captured frame is new (no repeats) and no gap is larger
    // than one missed capture tick. Repeats are only a failure if the game itself drew at
    // least the capture rate during the clip; when another app loads the GPU the test
    // game can fall below it, and then a capture tick without a new game frame repeats
    // the last one.
    let (first, last) = (report.first.unwrap(), report.last.unwrap());
    let game_times = sync::parse_log(&std::fs::read_to_string(&frames_log).unwrap());
    let allowed =
        continuity::allowed_repeats(&game_times, first, last, 1.0 / f64::from(capture_fps));
    println!(
        "test game drew {:.1} fps during the clip; its frame gaps allow {allowed} repeats",
        f64::from(last - first) / dur
    );
    if allowed > 0 {
        println!("WARNING: the test game stalled (GPU busy?), so some repeats are expected");
    }
    assert!(
        report.duplicates <= allowed,
        "repeated frames: {} with {allowed} justified by game stalls",
        report.duplicates
    );
    let big_gaps = report.steps_over(expected_step * 2 + 1);
    println!("steps over {}: {big_gaps}", expected_step * 2 + 1);
    assert_eq!(big_gaps, 0, "gaps longer than one capture tick");
}
