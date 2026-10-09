//! M6.4 acceptance: Klip's engine without a window (`klip-cli daemon`). The test game is
//! on the user's game list; the engine must notice it, buffer it, save a continuous clip
//! into its game folder when the clip hotkey is pressed, stop when the game exits, and
//! cost next to nothing while no game runs.
//!
//! Windows only for now: the hotkey is pressed with `SendInput` (a bare F24, which no
//! keyboard has and which the hotkey consumes, so nothing reaches the focused app).

#![cfg(windows)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_precision_loss
)]

mod common;

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::{KillOnDrop, cli, game, run_json, spawn_ready};
use klip_testkit::{continuity, oracle, sync};
use serde_json::{Value, json};

const GAME_FPS: u32 = 120;
const CAPTURE_FPS: u32 = 60;

/// The daemon's events, in order; ones not waited for yet are kept.
struct Events {
    rx: Receiver<Value>,
    backlog: VecDeque<Value>,
}

/// Spawn the daemon; every JSON line it prints becomes an event.
fn spawn_daemon(cmd: &mut Command) -> (KillOnDrop, Events) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(v);
            }
        }
    });
    (
        KillOnDrop(child),
        Events {
            rx,
            backlog: VecDeque::new(),
        },
    )
}

impl Events {
    /// The first event called `name` (earlier ones of other kinds stay queued); fails on
    /// `recorder_failed` and `action_failed`.
    fn wait_for(&mut self, name: &str, timeout: Duration) -> Value {
        if let Some(i) = self.backlog.iter().position(|e| e["event"] == name) {
            return self.backlog.remove(i).unwrap();
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let e = self
                .rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no {name} event within {timeout:?}"));
            println!("event: {e}");
            let kind = e["event"].as_str().unwrap_or_default();
            assert!(
                kind != "recorder_failed" && kind != "action_failed",
                "while waiting for {name}: {e}"
            );
            if kind == name {
                return e;
            }
            self.backlog.push_back(e);
        }
    }
}

/// CPU used by `pid` over `secs`, in percent of one core.
fn cpu_percent(pid: u32, secs: u64) -> f64 {
    let a = klip_platform::process_usage(pid).unwrap();
    std::thread::sleep(Duration::from_secs(secs));
    let b = klip_platform::process_usage(pid).unwrap();
    (b.cpu_ns - a.cpu_ns) as f64 / (secs as f64 * 1e9) * 100.0
}

fn write_settings(path: &Path, clips: &Path) {
    let settings = json!({
        "app": {"control_channel": true},
        "clips": {"folder": clips, "per_game_folders": true},
        "recording": {
            "buffer_secs": 30,
            "quality": {"preset": "custom", "custom": {
                "max_width": 1280, "max_height": 720, "fps": CAPTURE_FPS, "bitrate_kbps": 20000,
            }},
        },
        "audio": {"desktop": "game", "voice": false, "mic": false},
        "hotkeys": [{"action": "clip.save", "keys": "F24", "secs": 10}],
        "games": {
            "buffer": "games",
            // Only the test game: never a game the human is playing.
            "detect_stores": false,
            "user_games": [{"exe": "klip-testgame.exe", "name": "Klip Test Game"}],
        },
    });
    std::fs::write(path, serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
}

#[test]
#[ignore = "hardware: needs GPU encoder, desktop session, audio output, ffmpeg; presses F24; run via `cargo xtask e2e`"]
fn daemon_follows_a_game_and_clips_on_the_hotkey() {
    assert!(
        oracle::available("ffprobe") && oracle::available("ffmpeg"),
        "ffmpeg/ffprobe must be on PATH"
    );
    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let settings = tmp.path().join("settings.json");
    let clips = tmp.path().join("clips");
    write_settings(&settings, &clips);

    let (mut daemon, mut events) = spawn_daemon(
        cli(&discovery)
            .arg("daemon")
            .arg("--settings")
            .arg(&settings),
    );
    let ready = events.wait_for("ready", Duration::from_secs(60));
    assert_eq!(ready["settings"], "loaded", "{ready}");
    let hotkeys = events.wait_for("hotkeys", Duration::from_secs(5));
    assert_eq!(hotkeys["bound"][0]["error"], Value::Null, "{hotkeys}");
    let pid = daemon.0.id();

    // No game yet: the engine only polls (after start-up has settled).
    std::thread::sleep(Duration::from_secs(4));
    let idle = cpu_percent(pid, 6);
    let mem = klip_platform::process_usage(pid).unwrap();
    println!(
        "idle, no game: CPU {idle:.2} % of one core, resident {:.1} MB, private {:.1} MB",
        mem.resident_bytes as f64 / 1e6,
        mem.private_bytes as f64 / 1e6
    );
    let out = cli(&discovery)
        .args(["clip", "--last", "5s", "--timeout", "2s"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("no game is running"),
        "{err}"
    );

    // The game starts: detected, buffered.
    let frames_log = tmp.path().join("frames.csv");
    let (game_child, game_ready) = spawn_ready(
        game()
            .args(["--background", "--fps", &GAME_FPS.to_string()])
            .args(["--width", "1280", "--height", "720", "--duration", "90s"])
            .arg("--frames-log")
            .arg(&frames_log),
        Duration::from_secs(30),
    );
    let mut game_proc = KillOnDrop(game_child);
    println!("game: {game_ready}");
    let started = Instant::now();
    let g = events.wait_for("game_started", Duration::from_secs(10));
    println!("detected after {:?}", started.elapsed());
    assert_eq!(g["game"]["name"], "Klip Test Game");
    assert_eq!(g["game"]["source"]["kind"], "user");
    assert_eq!(g["game"]["window"], game_ready["hwnd"]);
    let rec = events.wait_for("recorder_started", Duration::from_secs(60));
    assert_eq!(
        (rec["width"].as_u64(), rec["height"].as_u64()),
        (Some(1280), Some(720))
    );
    assert!(
        rec["clip_dir"]
            .as_str()
            .unwrap()
            .ends_with("Klip Test Game"),
        "{rec}"
    );

    // The clip hotkey saves the last 10 s into the game's folder.
    std::thread::sleep(Duration::from_secs(13));
    klip_platform::press_keys("F24").unwrap();
    let saved = events.wait_for("clip_saved", Duration::from_secs(15));
    assert_eq!(saved["game"], "Klip Test Game");
    assert_eq!(saved["window"], game_ready["hwnd"]);
    let path = Path::new(saved["clip"]["path"].as_str().unwrap()).to_path_buf();
    assert_eq!(
        path.parent().unwrap(),
        clips.join("Klip Test Game"),
        "clip in the game's folder"
    );
    let ms = saved["clip"]["duration_ms"].as_i64().unwrap();
    assert!((9_000..=11_000).contains(&ms), "{ms} ms");

    let status = run_json(cli(&discovery).arg("status"));
    assert!(status["encoder"].is_string(), "{status}");

    // Let the game write its frame log, then end it: the engine stops buffering.
    std::thread::sleep(Duration::from_millis(1500));
    let _ = game_proc.0.kill();
    let _ = game_proc.0.wait();
    let stopped_at = Instant::now();
    events.wait_for("recorder_stopped", Duration::from_secs(10));
    events.wait_for("game_stopped", Duration::from_secs(5));
    println!("stopped {:?} after the game exited", stopped_at.elapsed());
    let after = cpu_percent(pid, 6);
    let mem = klip_platform::process_usage(pid).unwrap();
    println!(
        "idle after the game: CPU {after:.2} % of one core, resident {:.1} MB, private {:.1} MB",
        mem.resident_bytes as f64 / 1e6,
        mem.private_bytes as f64 / 1e6
    );
    assert!(
        idle < 1.0 && after < 1.0,
        "idle CPU {idle:.2} % / {after:.2} %"
    );

    // The clip: H.264 + the game's audio, every frame once (as in M1).
    let streams = oracle::probe_streams(&path).unwrap();
    assert_eq!(streams.len(), 2, "{streams:?}");
    assert_eq!(streams[0].codec_name.as_deref(), Some("h264"));
    assert_eq!(
        streams[1].tags.get("handler_name").map(String::as_str),
        Some("Game")
    );
    let counters = oracle::decode_counters(&path, 640, 360).unwrap();
    let report = continuity::analyze(&counters);
    println!("continuity: {report}");
    assert_eq!(report.unreadable, 0, "unreadable frames");
    assert_eq!(report.backwards, 0, "frames out of order");
    let (first, last) = (report.first.unwrap(), report.last.unwrap());
    let game_times = sync::parse_log(&std::fs::read_to_string(&frames_log).unwrap());
    let allowed =
        continuity::allowed_repeats(&game_times, first, last, 1.0 / f64::from(CAPTURE_FPS));
    if allowed > 0 {
        println!("WARNING: the test game stalled (GPU busy?): {allowed} repeats allowed");
    }
    assert!(report.duplicates <= allowed, "repeats: {report}");
    assert_eq!(
        report.steps_over((GAME_FPS / CAPTURE_FPS) * 2 + 1),
        0,
        "gaps: {report}"
    );

    // `klip-cli stop` ends the daemon.
    let _ = cli(&discovery).arg("stop").status();
    let deadline = Instant::now() + Duration::from_secs(15);
    while daemon.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the daemon did not stop");
        std::thread::sleep(Duration::from_millis(100));
    }
}
