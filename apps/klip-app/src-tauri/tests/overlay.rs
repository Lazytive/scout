//! M6.9 acceptance: the overlay pop-up shows for its time and never takes the focus, and
//! what it costs the game's frame pacing. Everything runs off-screen (the app's window,
//! the overlay, the test game) and silently, so nothing appears on anyone's monitors.

#![cfg(windows)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn exe_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_klip-app"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn connect(discovery: &Path) -> klip_control::Client {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(mut c) = klip_control::Client::connect(discovery, Duration::from_secs(1))
            && c.call("ping", Value::Null).is_ok()
        {
            return c;
        }
        assert!(Instant::now() < deadline, "the app did not start");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn overlay_window() -> Option<u64> {
    klip_platform::find_windows("Klip overlay")
        .ok()?
        .into_iter()
        .next()
        .map(|w| w.handle)
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

#[test]
#[ignore = "e2e: runs the app (off-screen); run via `cargo xtask e2e`"]
fn popup_shows_its_time_and_never_takes_focus() {
    klip_platform::enable_dpi_awareness();
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    let discovery = tmp.path().join("control.json");
    let frames = tmp.path().join("frames.csv");
    std::fs::write(
        &settings,
        json!({
            "app": {"control_channel": true, "first_run_done": true, "start_minimized": true},
            "clips": {"folder": tmp.path().join("clips")},
            "hotkeys": [],
            "games": {"buffer": "off"},
            "overlay": {"mode": "minimal", "seconds": 2, "sound": false, "notification": false},
        })
        .to_string(),
    )
    .unwrap();

    // The test game, off-screen, logging every frame.
    let game_exe = exe_dir().join("klip-testgame.exe");
    assert!(
        game_exe.exists(),
        "build klip-testgame first (cargo xtask e2e does)"
    );
    let mut game = Kill(
        Command::new(&game_exe)
            .args(["--background", "--position", "12000,900", "--fps", "120"])
            .args(["--width", "1280", "--height", "720", "--duration", "60s"])
            .arg("--frames-log")
            .arg(&frames)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let out = BufReader::new(game.0.stdout.take().unwrap());
    let started = Instant::now();
    for line in out.lines().map_while(Result::ok) {
        if line.contains("\"ready\"") {
            break;
        }
    }
    let game_t0 = started;

    let _app = Kill(
        Command::new(env!("CARGO_BIN_EXE_klip-app"))
            .env("KLIP_SETTINGS", &settings)
            .env("KLIP_DISCOVERY", &discovery)
            .env("KLIP_WINDOW_POS", "12000,100")
            .env("KLIP_OVERLAY_POS", "12000,1800")
            .env("KLIP_NO_FOCUS", "1")
            .spawn()
            .unwrap(),
    );
    let mut c = connect(&discovery);
    std::thread::sleep(Duration::from_secs(3));
    let baseline_end = game_t0.elapsed();

    let mut appear = Vec::new();
    let mut shown_for = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        c.call("overlay.preview", Value::Null).unwrap();
        let hwnd = loop {
            if let Some(h) = overlay_window() {
                break h;
            }
            assert!(
                t.elapsed() < Duration::from_secs(2),
                "the overlay did not show"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        appear.push(t.elapsed());
        let info = klip_platform::window_info(hwnd).unwrap();
        assert_eq!(
            (info.client_x, info.client_y),
            (12000, 1800),
            "placed by KLIP_OVERLAY_POS"
        );
        let shown = Instant::now();
        while klip_platform::window_visible(hwnd) {
            assert_ne!(
                klip_platform::foreground_window(),
                Some(hwnd),
                "the overlay took the focus"
            );
            assert!(
                shown.elapsed() < Duration::from_secs(5),
                "the overlay did not hide"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        shown_for.push(shown.elapsed());
        std::thread::sleep(Duration::from_millis(500));
    }
    let overlay_end = game_t0.elapsed();
    println!("overlay appeared after {appear:?}, shown for {shown_for:?}");
    for d in &shown_for {
        assert!(
            (Duration::from_millis(1700)..Duration::from_millis(2800)).contains(d),
            "{d:?} instead of ~2 s"
        );
    }
    assert!(
        appear.iter().all(|a| *a < Duration::from_millis(500)),
        "{appear:?}"
    );

    c.call("app.quit", Value::Null).unwrap();
    // The game writes its frame log once a second.
    std::thread::sleep(Duration::from_millis(1500));
    drop(game);

    // Frame pacing: baseline (after warm-up) vs while the pop-ups showed.
    let log = std::fs::read_to_string(&frames).unwrap();
    let times: Vec<f64> = log
        .lines()
        .filter_map(|l| l.split(',').nth(1)?.trim().parse::<f64>().ok())
        .map(|ns| ns / 1e9)
        .collect();
    let gaps = |from: f64, to: f64| -> Vec<f64> {
        times
            .windows(2)
            .filter(|w| w[0] >= from && w[1] <= to)
            .map(|w| (w[1] - w[0]) * 1000.0)
            .collect()
    };
    let mut base = gaps(1.0, baseline_end.as_secs_f64());
    let mut during = gaps(baseline_end.as_secs_f64(), overlay_end.as_secs_f64());
    assert!(
        base.len() > 100 && during.len() > 300,
        "{} {}",
        base.len(),
        during.len()
    );
    let (b50, b99, bmax) = (
        percentile(&mut base, 0.5),
        percentile(&mut base, 0.99),
        percentile(&mut base, 1.0),
    );
    let (d50, d99, dmax) = (
        percentile(&mut during, 0.5),
        percentile(&mut during, 0.99),
        percentile(&mut during, 1.0),
    );
    println!(
        "frame intervals (ms) baseline p50 {b50:.2} p99 {b99:.2} max {bmax:.2} | with pop-ups p50 {d50:.2} p99 {d99:.2} max {dmax:.2}"
    );
    assert!(dmax < 250.0, "a {dmax:.0} ms hitch while the pop-up showed");
}
