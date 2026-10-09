//! `cargo xtask bench` (M6.10): what Klip costs a game, measured on the benchmark scene
//! (the test game at 1920x1080 with a GPU load calibrated to ~240 fps uncapped), with
//! the app's engine (`klip-cli daemon`) on the Balanced preset (1080p60, 20 Mbit/s, game
//! audio, 2 min buffer). Two scenarios:
//! - **uncapped**: the game takes the whole GPU (the worst case: anything Klip does on
//!   the GPU shows up as lost frames);
//! - **capped** at 144 fps: the usual case (frame cap or vsync), where the GPU has
//!   headroom; does the game still hold 144, and how does its frame-time p99 change?
//!
//! GPU boost clocks and other desktop work drift by several percent between runs, more
//! than the effect measured, so each scenario is one long game run in which the replay
//! buffer is switched off and on (`settings.set games.buffer`) several times; each "on"
//! stretch is compared with the "off" stretch just before it. Then three 30 s saves and
//! the idle cost after the game closed. The test game window sits off-screen. Needs an
//! idle GPU. Writes `target/bench/bench-<unix time>.json` and prints a table.

#![cfg(windows)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

mod common;

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{KillOnDrop, cli, exe_dir};
use serde_json::{Value, json};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const CAP_FPS: u32 = 144;

/// The scene: uncapped (`cap: None`) or paced at `cap` fps.
fn game(load: u32, secs: u64, frames: &Path, cap: Option<u32>) -> (KillOnDrop, Instant) {
    let pacing = match cap {
        Some(fps) => vec!["--fps".to_string(), fps.to_string()],
        None => vec!["--uncapped".to_string()],
    };
    let child = Command::new(exe_dir().join("klip-testgame.exe"))
        .args(["--background", "--position", "12000,200"])
        .args(&pacing)
        .args([
            "--width",
            &WIDTH.to_string(),
            "--height",
            &HEIGHT.to_string(),
        ])
        .args(["--gpu-load", &load.to_string()])
        .args(["--duration", &format!("{secs}s")])
        .arg("--frames-log")
        .arg(frames)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    (KillOnDrop(child), Instant::now())
}

fn wait_exit(c: &mut Child, max: Duration) {
    let t = Instant::now();
    while c.try_wait().unwrap().is_none() {
        assert!(t.elapsed() < max, "the test game did not finish");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Frame times (s since the game started) from a frames log.
fn frame_times(path: &Path) -> Vec<f64> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split(',').nth(1)?.trim().parse::<f64>().ok())
        .map(|ns| ns / 1e9)
        .collect()
}

#[derive(Clone, Copy)]
struct Pace {
    fps: f64,
    p99_ms: f64,
}

/// Average fps and 99th-percentile frame time between `from` and `to` (s, game time).
fn pace(times: &[f64], from: f64, to: f64) -> Pace {
    let t: Vec<f64> = times
        .iter()
        .copied()
        .filter(|x| *x >= from && *x <= to)
        .collect();
    assert!(t.len() > 100, "only {} frames in {from}..{to} s", t.len());
    let mut gaps: Vec<f64> = t.windows(2).map(|w| (w[1] - w[0]) * 1000.0).collect();
    gaps.sort_by(f64::total_cmp);
    Pace {
        fps: (t.len() - 1) as f64 / (t[t.len() - 1] - t[0]),
        p99_ms: gaps[((gaps.len() - 1) as f64 * 0.99) as usize],
    }
}

struct Events {
    rx: Receiver<Value>,
    backlog: VecDeque<Value>,
}

impl Events {
    fn wait_for(&mut self, name: &str, timeout: Duration) -> Value {
        if let Some(i) = self.backlog.iter().position(|e| e["event"] == name) {
            return self.backlog.remove(i).unwrap();
        }
        let deadline = Instant::now() + timeout;
        loop {
            let e = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("no {name} event"));
            assert_ne!(e["event"], "recorder_failed", "{e}");
            if e["event"] == name {
                return e;
            }
            self.backlog.push_back(e);
        }
    }
}

/// CPU (% of one core) and peak memory of `pid`, sampled once a second for `secs`.
fn sample(pid: u32, secs: u64) -> (f64, u64, u64) {
    let a = klip_platform::process_usage(pid).unwrap();
    let mut resident = 0;
    let mut private = 0;
    for _ in 0..secs {
        std::thread::sleep(Duration::from_secs(1));
        let u = klip_platform::process_usage(pid).unwrap();
        resident = resident.max(u.resident_bytes);
        private = private.max(u.private_bytes);
    }
    let b = klip_platform::process_usage(pid).unwrap();
    let cpu = (b.cpu_ns - a.cpu_ns) as f64 / (secs as f64 * 1e9) * 100.0;
    (cpu, resident, private)
}

fn cpu_name() -> String {
    let out = Command::new("reg")
        .args([
            "query",
            r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "/v",
            "ProcessorNameString",
        ])
        .output()
        .ok();
    out.and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .find(|l| l.contains("ProcessorNameString"))
            .and_then(|l| l.split("REG_SZ").nth(1))
            .map(|s| s.trim().to_string())
    })
    .unwrap_or_default()
}

/// The daemon and how to drive it.
struct Klip {
    events: Events,
    discovery: std::path::PathBuf,
    pid: u32,
    _child: KillOnDrop,
}

impl Klip {
    fn buffer(&self, mode: &str) {
        let out = cli(&self.discovery)
            .args([
                "call",
                "settings.set",
                &json!({"games": {"buffer": mode}}).to_string(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// One scenario's measurements.
struct Scenario {
    pairs: Vec<(Pace, Pace)>,
    cpu: Vec<f64>,
    resident: u64,
    private: u64,
    rec: Value,
}

impl Scenario {
    /// Mean, median and 95 % interval of the fps cost (%) over the pairs.
    fn cost(&self) -> (f64, f64, f64, Vec<f64>) {
        let costs: Vec<f64> = self
            .pairs
            .iter()
            .map(|(off, on)| (off.fps - on.fps) / off.fps * 100.0)
            .collect();
        let n = costs.len() as f64;
        let mean = costs.iter().sum::<f64>() / n;
        let mut sorted = costs.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        let sd =
            (costs.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt();
        (mean, median, 1.96 * sd / n.sqrt(), costs)
    }

    fn mean(&self, f: impl Fn(&(Pace, Pace)) -> f64) -> f64 {
        self.pairs.iter().map(f).sum::<f64>() / self.pairs.len() as f64
    }
}

/// Run the scene and switch the buffer off/on `n` times; leaves the buffer on and the
/// game running (returned) so the caller can save clips.
fn measure(
    klip: &mut Klip,
    load: u32,
    cap: Option<u32>,
    n: usize,
    frames: &Path,
) -> (Scenario, KillOnDrop, Instant, f64) {
    let (game, spawned) = game(load, 600, frames, cap);
    let now = || spawned.elapsed().as_secs_f64();
    std::thread::sleep(Duration::from_secs(3));
    let mut windows = Vec::new();
    let mut sc = Scenario {
        pairs: Vec::new(),
        cpu: Vec::new(),
        resident: 0,
        private: 0,
        rec: Value::Null,
    };
    let mut started_at = 0.0;
    for pair in 0..n {
        if pair > 0 {
            klip.buffer("off");
            klip.events
                .wait_for("recorder_stopped", Duration::from_secs(20));
        }
        std::thread::sleep(Duration::from_secs(3));
        let off = (now(), now() + 6.0);
        std::thread::sleep(Duration::from_secs(6));
        klip.buffer("games");
        sc.rec = klip
            .events
            .wait_for("recorder_started", Duration::from_secs(60));
        started_at = now();
        std::thread::sleep(Duration::from_secs(3));
        let from = now();
        let (cpu, resident, private) = sample(klip.pid, 6);
        sc.cpu.push(cpu);
        sc.resident = sc.resident.max(resident);
        sc.private = sc.private.max(private);
        windows.push((off, (from, now())));
    }
    // The game flushes its frame log once a second.
    std::thread::sleep(Duration::from_millis(1500));
    let times = frame_times(frames);
    // Game time starts a little after the spawn; the windows have a margin for that.
    for ((o0, o1), (n0, n1)) in windows {
        sc.pairs.push((
            pace(&times, o0 + 0.3, o1 - 0.3),
            pace(&times, n0 + 0.3, n1 - 0.3),
        ));
    }
    (sc, game, spawned, started_at)
}

fn print_scenario(name: &str, sc: &Scenario) {
    let (mean, median, ci, costs) = sc.cost();
    println!(
        "{name}: off {:.1} fps (p99 {:.2} ms), on {:.1} fps (p99 {:.2} ms) -> cost {mean:.2} % +/- {ci:.2} (95 %), median {median:.2} %",
        sc.mean(|p| p.0.fps),
        sc.mean(|p| p.0.p99_ms),
        sc.mean(|p| p.1.fps),
        sc.mean(|p| p.1.p99_ms),
    );
    println!("  pairs: {costs:.1?}");
}

fn scenario_json(sc: &Scenario) -> Value {
    let (mean, median, ci, costs) = sc.cost();
    json!({
        "off_fps": sc.mean(|p| p.0.fps), "on_fps": sc.mean(|p| p.1.fps),
        "off_p99_ms": sc.mean(|p| p.0.p99_ms), "on_p99_ms": sc.mean(|p| p.1.p99_ms),
        "cost_percent_mean": mean, "cost_percent_median": median, "cost_percent_ci95": ci,
        "cost_percent_pairs": costs,
        "engine_cpu_percent_of_one_core": sc.cpu.iter().sum::<f64>() / sc.cpu.len() as f64,
        "engine_resident_bytes": sc.resident, "engine_private_bytes": sc.private,
    })
}

#[test]
#[ignore = "benchmark: needs an idle GPU; run via `cargo xtask bench`"]
fn bench_game_cost() {
    klip_platform::enable_dpi_awareness();
    let tmp = tempfile::tempdir().unwrap();
    let env_num = |k: &str, d: f64| {
        std::env::var(k)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(d)
    };
    let target_fps = env_num("KLIP_BENCH_FPS", 240.0);
    let pairs = env_num("KLIP_BENCH_PAIRS", 8.0) as usize;

    // Calibrate the GPU load: frame time is about linear in the loop steps.
    let probe = |load: u32, name: &str| {
        let frames = tmp.path().join(format!("{name}.csv"));
        let (mut g, _) = game(load, 4, &frames, None);
        wait_exit(&mut g.0, Duration::from_secs(20));
        pace(&frame_times(&frames), 1.5, 3.8).fps
    };
    let (f0, f1) = (probe(0, "cal0"), probe(1000, "cal1"));
    let per_step = (1.0 / f1 - 1.0 / f0) / 1000.0;
    let load = ((1.0 / target_fps - 1.0 / f0) / per_step).round().max(0.0) as u32;
    println!("calibration: {f0:.0} fps at load 0, {f1:.0} fps at 1000 -> load {load}");

    // The engine, Balanced preset, replay buffer off to begin with.
    let settings = tmp.path().join("settings.json");
    let discovery = tmp.path().join("control.json");
    std::fs::write(
        &settings,
        json!({
            "app": {"control_channel": true},
            "clips": {"folder": tmp.path().join("clips")},
            "audio": {"desktop": "game", "voice": false, "mic": false},
            "hotkeys": [],
            "storage": {"cap_gb": 0},
            "games": {"buffer": "off", "user_games": [{"exe": "klip-testgame.exe"}]},
        })
        .to_string(),
    )
    .unwrap();
    let mut child = cli(&discovery)
        .arg("daemon")
        .arg("--settings")
        .arg(&settings)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(v);
            }
        }
    });
    let mut klip = Klip {
        events: Events {
            rx,
            backlog: VecDeque::new(),
        },
        discovery: discovery.clone(),
        pid,
        _child: KillOnDrop(child),
    };
    klip.events.wait_for("ready", Duration::from_secs(60));
    std::thread::sleep(Duration::from_secs(3));
    let (idle_before_cpu, _, idle_before_private) = sample(pid, 5);

    // Worst case: uncapped.
    let (uncapped, mut g, spawned, started_at) = measure(
        &mut klip,
        load,
        None,
        pairs,
        &tmp.path().join("uncapped.csv"),
    );
    print_scenario("uncapped", &uncapped);
    let status = cli(&discovery).arg("status").output().unwrap();
    let status: Value = serde_json::from_slice(&status.stdout).unwrap_or(Value::Null);
    // Let the buffer reach 30 s, then save three 30 s clips.
    while spawned.elapsed().as_secs_f64() < started_at + 33.0 {
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut saves = Vec::new();
    for _ in 0..3 {
        let out = cli(&discovery)
            .args(["clip", "--last", "30s"])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        saves.push((
            v["save_ms"].as_f64().unwrap(),
            v["duration_ms"].as_f64().unwrap(),
            v["bytes"].as_u64().unwrap(),
        ));
        std::thread::sleep(Duration::from_secs(2));
    }
    let _ = g.0.kill();
    let _ = g.0.wait();
    klip.events
        .wait_for("recorder_stopped", Duration::from_secs(15));

    // Usual case: capped at 144 fps.
    klip.buffer("off");
    let (capped, mut g, _, _) = measure(
        &mut klip,
        load,
        Some(CAP_FPS),
        (pairs / 2).max(2),
        &tmp.path().join("capped.csv"),
    );
    print_scenario("capped at 144", &capped);
    let _ = g.0.kill();
    let _ = g.0.wait();
    klip.events
        .wait_for("recorder_stopped", Duration::from_secs(15));
    std::thread::sleep(Duration::from_secs(3));
    let (idle_after_cpu, idle_after_resident, idle_after_private) = sample(pid, 10);
    let _ = cli(&discovery).arg("stop").status();

    let cores = std::thread::available_parallelism().map_or(1, usize::from) as f64;
    let mut save_ms: Vec<f64> = saves.iter().map(|s| s.0).collect();
    save_ms.sort_by(f64::total_cmp);
    let gpu = klip_platform::list_gpus()
        .unwrap_or_default()
        .into_iter()
        .find(|g| !g.software)
        .map(|g| (g.name, g.driver_version))
        .unwrap_or_default();
    let rec = &uncapped.rec;
    let report = json!({
        "when_unix": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "machine": {
            "cpu": cpu_name(),
            "logical_cores": cores,
            "gpu": gpu.0,
            "gpu_driver": gpu.1.map(|v| format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xffff, (v >> 16) & 0xffff, v & 0xffff)),
        },
        "scene": {"width": WIDTH, "height": HEIGHT, "gpu_load": load, "uncapped_target_fps": target_fps, "capped_fps": CAP_FPS, "window": "off-screen, windowed"},
        "settings": {"preset": "balanced", "encoder": rec["encoder"], "output": [rec["width"], rec["height"]], "fps": rec["fps"], "buffer_s": 120, "audio": "game"},
        "uncapped": scenario_json(&uncapped),
        "capped": scenario_json(&capped),
        "capture": status.get("video").cloned().unwrap_or(Value::Null),
        "saves_30s": saves.iter().map(|s| json!({"save_ms": s.0, "duration_ms": s.1, "bytes": s.2})).collect::<Vec<_>>(),
        "ring_bytes_after_9s": status["bytes"],
        "engine_idle": {
            "before_game_cpu_percent": idle_before_cpu, "before_game_private_bytes": idle_before_private,
            "after_game_cpu_percent": idle_after_cpu, "after_game_resident_bytes": idle_after_resident, "after_game_private_bytes": idle_after_private,
        },
    });
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("bench-{}.json", report["when_unix"]));
    std::fs::write(&file, serde_json::to_vec_pretty(&report).unwrap()).unwrap();

    let mb = |b: u64| b as f64 / 1e6;
    let u = scenario_json(&uncapped);
    println!("\n=== Klip benchmark ({}) ===", file.display());
    println!(
        "machine: {} / {} (driver {})",
        report["machine"]["cpu"], report["machine"]["gpu"], report["machine"]["gpu_driver"]
    );
    println!(
        "scene: {WIDTH}x{HEIGHT}, gpu load {load}, off-screen window; Klip: Balanced, {} {}x{}@{}, game audio, 2 min buffer",
        rec["encoder"], rec["width"], rec["height"], rec["fps"]
    );
    print_scenario("uncapped (worst case)", &uncapped);
    print_scenario("capped at 144 (usual)", &capped);
    println!(
        "engine while buffering: CPU {:.1} % of one core ({:.2} % of all {cores:.0}), resident {:.0} MB, private {:.0} MB",
        u["engine_cpu_percent_of_one_core"].as_f64().unwrap_or(0.0),
        u["engine_cpu_percent_of_one_core"].as_f64().unwrap_or(0.0) / cores,
        mb(uncapped.resident),
        mb(uncapped.private)
    );
    println!("30 s saves: {save_ms:.1?} ms");
    println!(
        "engine idle: before a game CPU {idle_before_cpu:.2} %, private {:.0} MB; after CPU {idle_after_cpu:.2} %, private {:.0} MB (resident {:.0} MB)",
        mb(idle_before_private),
        mb(idle_after_private),
        mb(idle_after_resident)
    );
    println!("capture: {}", report["capture"]);
}
