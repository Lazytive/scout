//! M3.5 acceptance: capture on a monitor in Windows HDR mode.
//!
//! - An SDR game on the HDR desktop comes out unchanged (its gray patches and checker
//!   keep their 8-bit values).
//! - An HDR game (test game `--hdr`, FP16 scRGB, paper white = the monitor's SDR white)
//!   keeps its SDR-range parts, and its highlight patches stay distinct and below white
//!   up to the display's peak instead of clipping at SDR white.
//!
//! The tests use the monitor the test game's window opens on; if it isn't in HDR mode
//! they print why and pass without checking. Run via `cargo xtask e2e`.

// Windows only: window/monitor/DXGI switching and Windows HDR mode (WGC, DXGI). On
// Linux the portal decides what is captured; HDR desktops there are on the M4.6 list.
#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{KillOnDrop, capture_api_args, cli, game, run_json, spawn_ready, stop_recorder};
use klip_testkit::{oracle, patches};
use serde_json::Value;

struct Run {
    /// Patch levels and the two checker levels (dark, light) of one decoded frame.
    patches: [f32; patches::COUNT],
    checker: (f32, f32),
    status: Value,
    monitor: klip_platform::MonitorInfo,
}

/// The monitor a window is on.
fn monitor_of(hwnd: u64) -> klip_platform::MonitorInfo {
    klip_platform::enable_dpi_awareness();
    let w = klip_platform::window_info(hwnd).unwrap();
    klip_platform::list_monitors()
        .unwrap()
        .into_iter()
        .find(|m| m.handle == w.monitor)
        .unwrap()
}

/// Most common dark and light gray in the checker area (rows 20–70 %).
fn checker_levels(frame: &[u8], w: usize, h: usize) -> (f32, f32) {
    let mut hist = [0usize; 256];
    for y in (h / 5..h * 7 / 10).step_by(3) {
        for x in (0..w).step_by(3) {
            hist[frame[y * w + x] as usize] += 1;
        }
    }
    let mut top: Vec<(usize, usize)> = hist.iter().copied().enumerate().collect();
    top.sort_by_key(|e| std::cmp::Reverse(e.1));
    let (a, b) = (top[0].0 as f32, top[1].0 as f32);
    (a.min(b), a.max(b))
}

/// Run the test game with `game_args`, record it, save a 3 s clip, decode one frame.
/// `None` if the game's monitor isn't in HDR mode.
fn record(game_args: &[String]) -> Option<Run> {
    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let clips = tmp.path().join("clips");
    let (game, ready) = spawn_ready(
        game()
            .args([
                "--background",
                "--fps",
                "60",
                "--width",
                "1280",
                "--height",
                "720",
            ])
            .args(["--duration", "40s"])
            .args(game_args),
        Duration::from_secs(30),
    );
    let _game = KillOnDrop(game);
    let hwnd = ready["hwnd"].as_u64().unwrap();
    let monitor = monitor_of(hwnd);
    if !monitor.hdr {
        println!("skipped: the test game's monitor is not in HDR mode ({monitor:?})");
        return None;
    }
    let (rec, rec_ready) = spawn_ready(
        cli(&discovery)
            .args([
                "record", "--buffer", "20s", "--fps", "60", "--audio", "none",
            ])
            .args(["--window-handle", &hwnd.to_string()])
            .args(capture_api_args())
            .arg("--out")
            .arg(&clips),
        Duration::from_secs(60),
    );
    let mut rec = KillOnDrop(rec);
    println!("recorder: {rec_ready}");
    // Long enough for the scene-peak estimate to settle (it rises within a few frames).
    std::thread::sleep(Duration::from_secs(5));
    let saved = run_json(cli(&discovery).args(["clip", "--last", "3s"]));
    let status = run_json(cli(&discovery).arg("status"));
    stop_recorder(&discovery, &mut rec);
    let path = PathBuf::from(saved["path"].as_str().unwrap());
    let (w, h) = (1280usize, 720usize);
    let frame = oracle::decode_gray_frame(&path, 2.0, w, h).unwrap();
    Some(Run {
        patches: patches::read_gray(&frame, w, h).unwrap(),
        checker: checker_levels(&frame, w, h),
        status,
        monitor,
    })
}

#[test]
#[ignore = "hardware: needs a monitor in Windows HDR mode, GPU encoder, ffmpeg; run via `cargo xtask e2e`"]
fn sdr_game_on_hdr_desktop_is_unchanged() {
    let Some(run) = record(&["--patches".into()]) else {
        return;
    };
    println!("capture: {}", run.status["capture"]);
    println!("patches {:?}, checker {:?}", run.patches, run.checker);
    assert_eq!(run.status["capture_api"], "Klip");
    for (got, want) in run.patches.iter().zip(patches::SDR_CODES) {
        assert!((got - f32::from(want)).abs() <= 2.5, "patch {want}: {got}");
    }
    assert!((run.checker.0 - 60.0).abs() <= 2.5, "{:?}", run.checker);
    assert!((run.checker.1 - 90.0).abs() <= 2.5, "{:?}", run.checker);
    // No HDR content: the scene peak stays at SDR white.
    let peak = run.status["capture"]["scene_peak_nits"].as_f64().unwrap();
    let white = f64::from(run.monitor.sdr_white_nits.unwrap_or(80.0));
    assert!(
        (peak - white).abs() < white * 0.05,
        "scene peak {peak} vs SDR white {white}"
    );
}

#[test]
#[ignore = "hardware: needs a monitor in Windows HDR mode, GPU encoder, ffmpeg; run via `cargo xtask e2e`"]
fn hdr_game_is_tone_mapped() {
    // Probe the monitor first so the game's paper white can match its SDR white.
    let probe = record(&["--patches".into()]);
    let Some(probe) = probe else {
        return;
    };
    let white = probe.monitor.sdr_white_nits.unwrap_or(80.0);
    let peak = probe.monitor.max_nits.unwrap_or(1000.0);
    let Some(run) = record(&["--hdr".into(), "--paper-white".into(), format!("{white}")]) else {
        return;
    };
    println!("monitor: SDR white {white} nits, peak {peak} nits");
    println!("capture: {}", run.status["capture"]);
    println!(
        "patches {:?} for {:?} nits, checker {:?}",
        run.patches,
        patches::HDR_NITS,
        run.checker
    );
    // SDR-range parts (below the knee) are unchanged.
    assert!((run.checker.0 - 60.0).abs() <= 3.0, "{:?}", run.checker);
    assert!((run.checker.1 - 90.0).abs() <= 3.0, "{:?}", run.checker);
    // Highlights up to the display's peak stay distinct and below white; content
    // brighter than the display can show ends up at white.
    let mut prev = -1.0f32;
    for (level, nits) in run.patches.iter().zip(patches::HDR_NITS) {
        if nits <= peak * 0.9 {
            assert!(
                *level >= prev + 3.0,
                "patch {nits} nits ({level}) not above the previous ({prev})"
            );
            assert!(
                *level <= 253.5,
                "patch {nits} nits clipped to white ({level})"
            );
        } else if nits >= peak * 1.1 {
            assert!(
                *level >= 250.0,
                "patch {nits} nits above the display's peak: {level}"
            );
        }
        prev = *level;
    }
    // The measured scene peak is the display's (the brightest patch is above it).
    let measured = run.status["capture"]["scene_peak_nits"].as_f64().unwrap();
    assert!(
        (measured - f64::from(peak)).abs() < f64::from(peak) * 0.1,
        "scene peak {measured} vs display {peak}"
    );
}
