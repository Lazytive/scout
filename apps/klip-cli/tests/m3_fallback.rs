//! M3.6 acceptance: when window capture stops delivering (here: the test game, borderless
//! fullscreen on a secondary monitor, stops drawing for 5 s), the recorder switches to
//! capturing that monitor, and the replay buffer carries on across the switch: one clip
//! holds frames from before (window capture) and after (monitor capture), in order.
//!
//! Exclusive fullscreen itself can't be tested without taking over a monitor (and focus);
//! its rule is unit-tested (`klip_recorder::supervisor`) and on the human checklist.
//! Run via `cargo xtask e2e`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{KillOnDrop, cli, game, game_monitor, run_json, spawn_ready, stop_recorder};
use klip_testkit::patches;
use klip_testkit::{continuity, oracle};

#[test]
#[ignore = "hardware: needs a desktop session, GPU encoder, ffmpeg; run via `cargo xtask e2e`"]
fn stalled_window_capture_switches_to_the_monitor() {
    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let clips = tmp.path().join("clips");
    let freeze = (8u64, 13u64);
    let t0 = Instant::now();
    let (game, ready) = spawn_ready(
        game().args([
            "--background",
            "--mode",
            "borderless",
            "--fps",
            "60",
            "--duration",
            "40s",
            "--freeze",
            &format!("{}s..{}s", freeze.0, freeze.1),
        ]),
        Duration::from_secs(30),
    );
    let _game = KillOnDrop(game);
    let hwnd = ready["hwnd"].as_u64().unwrap();
    println!("game: {ready}");
    let (rec, rec_ready) = spawn_ready(
        cli(&discovery)
            .args([
                "record", "--buffer", "30s", "--fps", "60", "--audio", "none",
            ])
            .args(["--no-cursor", "--capture-api", "klip"])
            .args(["--window-handle", &hwnd.to_string()])
            .arg("--out")
            .arg(&clips),
        Duration::from_secs(60),
    );
    let mut rec = KillOnDrop(rec);
    println!("recorder: {rec_ready}");
    // Until a few seconds after the game draws again.
    let until = Duration::from_secs(freeze.1 + 4);
    if let Some(left) = until.checked_sub(t0.elapsed()) {
        std::thread::sleep(left);
    }
    let saved = run_json(cli(&discovery).args(["clip", "--last", "14s"]));
    let status = run_json(cli(&discovery).arg("status"));
    stop_recorder(&discovery, &mut rec);
    println!("video: {}", status["video"]);
    println!("capture: {} {}", status["capture_api"], status["capture"]);

    let switches = status["video"]["switches"].as_array().unwrap();
    assert!(
        switches
            .iter()
            .any(|s| s["reason"] == "window capture stalled" && s["ok"] == true),
        "no switch to the monitor: {}",
        status["video"]
    );
    assert_eq!(status["video"]["mode"]["kind"], "window_on_monitor");

    let path = PathBuf::from(saved["path"].as_str().unwrap());
    let counters = oracle::decode_counters(&path, 640, 360).unwrap();
    let report = continuity::analyze(&counters);
    println!("continuity: {report}");
    assert_eq!(report.unreadable, 0, "unreadable frames");
    assert_eq!(report.backwards, 0, "frames out of order");
    // The game froze at ~freeze.0 s (~60 frames a second before that) and went on from
    // the same counter after freeze.1 s. Frames from both sides are in one clip.
    let frozen_at = (freeze.0 * 60) as u32;
    let first = report.first.unwrap();
    let last = report.last.unwrap();
    assert!(
        first < frozen_at.saturating_sub(60),
        "clip starts after the freeze: {first}"
    );
    assert!(last > frozen_at + 120, "nothing after the freeze: {last}");
}

#[test]
#[ignore = "hardware: needs a desktop session, GPU encoder, ffmpeg; run via `cargo xtask e2e`"]
fn dxgi_monitor_capture_records_the_game() {
    let Some(monitor) = game_monitor() else {
        println!("skipped: needs the test game on its own monitor (set KLIP_E2E_MONITOR)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let clips = tmp.path().join("clips");
    let (game, _ready) = spawn_ready(
        game().args([
            "--background",
            "--mode",
            "borderless",
            "--patches",
            "--fps",
            "60",
            "--duration",
            "30s",
        ]),
        Duration::from_secs(30),
    );
    let _game = KillOnDrop(game);
    let (rec, rec_ready) = spawn_ready(
        cli(&discovery)
            .args([
                "record", "--buffer", "20s", "--fps", "60", "--audio", "none",
            ])
            .args(["--no-cursor", "--capture-api", "gst-dxgi"])
            .args(["--monitor", &monitor.to_string()])
            .args(["--max-size", "1280x720"])
            .arg("--out")
            .arg(&clips),
        Duration::from_secs(60),
    );
    let mut rec = KillOnDrop(rec);
    println!("recorder: {rec_ready}");
    std::thread::sleep(Duration::from_secs(6));
    let saved = run_json(cli(&discovery).args(["clip", "--last", "4s"]));
    let status = run_json(cli(&discovery).arg("status"));
    stop_recorder(&discovery, &mut rec);
    assert_eq!(status["capture_api"], "GstDxgi", "{status}");
    let path = PathBuf::from(saved["path"].as_str().unwrap());
    let counters = oracle::decode_counters(&path, 640, 360).unwrap();
    let report = continuity::analyze(&counters);
    println!("continuity: {report}");
    assert_eq!(report.unreadable, 0, "unreadable frames");
    assert_eq!(report.backwards, 0, "frames out of order");
    assert!(report.frames >= 200, "too few frames: {report}");
    // SDR levels come through unchanged, also on an HDR desktop.
    let frame = oracle::decode_gray_frame(&path, 2.0, 1280, 720).unwrap();
    let levels = patches::read_gray(&frame, 1280, 720).unwrap();
    println!("patches {levels:?}");
    for (got, want) in levels.iter().zip(patches::SDR_CODES) {
        assert!((got - f32::from(want)).abs() <= 4.0, "patch {want}: {got}");
    }
}
