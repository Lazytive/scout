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

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use klip_testkit::{continuity, oracle};
use serde_json::Value;

fn exe_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_klip-cli"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Spawn a process and wait for its first stdout line that parses as JSON with
/// `"event":"ready"`. Remaining stdout is drained in the background.
fn spawn_ready(cmd: &mut Command, timeout: Duration) -> (Child, Value) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {cmd:?}: {e}"));
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && v["event"] == "ready"
            {
                let _ = tx.send(v);
            }
        }
    });
    match rx.recv_timeout(timeout) {
        Ok(v) => (child, v),
        Err(_) => {
            let _ = child.kill();
            panic!("{cmd:?} did not become ready within {timeout:?}");
        }
    }
}

fn cli(discovery: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_klip-cli"));
    c.arg("--discovery").arg(discovery);
    c
}

fn run_json(cmd: &mut Command) -> Value {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{cmd:?} printed non-JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
    let game_exe = exe_dir().join(if cfg!(windows) {
        "klip-testgame.exe"
    } else {
        "klip-testgame"
    });
    assert!(
        game_exe.exists(),
        "{} missing: build it (cargo xtask e2e does)",
        game_exe.display()
    );

    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let clips = tmp.path().join("clips");
    let game_fps = 120;
    let capture_fps = 60;

    let (game, ready) = spawn_ready(
        Command::new(&game_exe).args([
            "--fps",
            &game_fps.to_string(),
            "--width",
            "1280",
            "--height",
            "720",
            "--duration",
            &format!("{}s", clip_secs + 40),
        ]),
        Duration::from_secs(30),
    );
    let _game = KillOnDrop(game);
    let hwnd = ready["hwnd"].as_u64().unwrap();
    assert_ne!(hwnd, 0);

    let (rec, rec_ready) = spawn_ready(
        cli(&discovery)
            .args([
                "record",
                "--buffer",
                "120s",
                "--fps",
                &capture_fps.to_string(),
            ])
            .args(["--window-handle", &hwnd.to_string()])
            .arg("--out")
            .arg(&clips),
        Duration::from_secs(60),
    );
    let mut rec = KillOnDrop(rec);
    println!("recorder: {rec_ready}");

    std::thread::sleep(Duration::from_secs(clip_secs + 3));

    let t0 = Instant::now();
    let saved = run_json(cli(&discovery).args(["clip", "--last", &format!("{clip_secs}s")]));
    let cli_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let status = run_json(cli(&discovery).arg("status"));
    let _ = cli(&discovery).arg("stop").status();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if rec.0.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
    assert_eq!(streams.len(), 1, "{streams:?}");
    let v = &streams[0];
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
    // than one missed capture tick.
    assert_eq!(report.duplicates, 0, "repeated frames");
    let big_gaps = report.steps_over(expected_step * 2 + 1);
    println!("steps over {}: {big_gaps}", expected_step * 2 + 1);
    assert_eq!(big_gaps, 0, "gaps longer than one capture tick");
}
