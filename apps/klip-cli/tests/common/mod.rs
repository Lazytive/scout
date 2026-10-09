//! Helpers shared by the hardware acceptance tests.

#![allow(
    dead_code,
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
use std::time::Duration;

use serde_json::Value;

pub fn exe_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_klip-cli"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Spawn a process and wait for its first stdout line that parses as JSON with
/// `"event":"ready"`. Remaining stdout is drained in the background.
pub fn spawn_ready(cmd: &mut Command, timeout: Duration) -> (Child, Value) {
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

pub fn cli(discovery: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_klip-cli"));
    c.arg("--discovery").arg(discovery);
    c
}

/// Extra `record` arguments from `KLIP_E2E_CAPTURE_API` (e.g. `gst-wgc`), for A/B runs
/// of the capture implementations.
pub fn capture_api_args() -> Vec<String> {
    match std::env::var("KLIP_E2E_CAPTURE_API") {
        Ok(api) if !api.is_empty() => vec!["--capture-api".into(), api],
        _ => Vec::new(),
    }
}

pub fn run_json(cmd: &mut Command) -> Value {
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

pub struct KillOnDrop(pub Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The test game, placed on a secondary monitor when there is one, so test runs don't
/// cover the main screen (someone may be using it). Windows: `KLIP_E2E_MONITOR=<index>`
/// picks a monitor by its `klip-cli monitors` index; `KLIP_E2E_MONITOR=primary` keeps
/// Windows' default placement; `KLIP_E2E_MONITOR=offscreen` puts the window beyond all
/// monitors (window capture still works; nothing shows on anyone's screen). Linux: the game goes borderless fullscreen on
/// [`linux_output`] (Wayland lets no app place a window).
pub fn game() -> Command {
    let mut c = Command::new(game_exe());
    if cfg!(target_os = "linux") {
        c.arg("--monitor").arg(linux_output());
        return c;
    }
    if std::env::var("KLIP_E2E_MONITOR").as_deref() == Ok("offscreen") {
        c.arg("--position").arg("12000,200");
        return c;
    }
    let monitors = klip_platform::list_monitors().unwrap_or_default();
    if let Some(m) = game_monitor().and_then(|i| monitors.get(i)) {
        c.arg("--position").arg(format!(
            "{},{}",
            m.x.saturating_add(64),
            m.y.saturating_add(64)
        ));
    }
    c
}

/// Index (as `klip-cli monitors` lists them) of the monitor [`game`] opens the test game
/// on; `None` for Windows' default placement.
pub fn game_monitor() -> Option<usize> {
    klip_platform::enable_dpi_awareness();
    let monitors = klip_platform::list_monitors().unwrap_or_default();
    match std::env::var("KLIP_E2E_MONITOR").ok().as_deref() {
        Some("primary") => None,
        Some(i) => i.parse::<usize>().ok().filter(|i| *i < monitors.len()),
        None => monitors.iter().position(|m| !m.primary),
    }
}

/// Linux: the monitor the test game covers, `KLIP_E2E_OUTPUT` (e.g. `DP-3`) or
/// `secondary` (the test game's pick: a monitor that isn't at the desktop origin).
pub fn linux_output() -> String {
    std::env::var("KLIP_E2E_OUTPUT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "secondary".into())
}

/// Linux: where the portal restore token for the test monitor is kept, so the desktop's
/// dialog is answered once (pick the test game's monitor) and later runs are unattended.
/// `KLIP_E2E_PORTAL_TOKEN` overrides it; delete the file to pick again.
pub fn portal_token() -> PathBuf {
    std::env::var_os("KLIP_E2E_PORTAL_TOKEN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .unwrap()
                .join("klip-e2e")
                .join(format!("portal-{}.token", linux_output()))
        })
}

/// `record` arguments that capture the test game started with [`game`]: its window on
/// Windows (from the ready line's `hwnd`), the portal's monitor stream on Linux.
pub fn capture_args(game_ready: &Value) -> Vec<String> {
    if cfg!(target_os = "linux") {
        let token = portal_token();
        if !token.exists() {
            println!(
                "NOTE: no portal token at {}: pick the monitor the test game covers ({}) in \
                 the desktop's sharing dialog",
                token.display(),
                game_ready["monitor_name"].as_str().unwrap_or("?")
            );
        }
        return vec![
            "--portal".into(),
            "monitor".into(),
            "--portal-token".into(),
            token.to_string_lossy().into_owned(),
        ];
    }
    let hwnd = game_ready["hwnd"].as_u64().unwrap();
    assert_ne!(hwnd, 0);
    vec!["--window-handle".into(), hwnd.to_string()]
}

/// Linux: fail early, with what to do, when the portal stream isn't the monitor the test
/// game covers (e.g. KDE's "Full Workspace" was picked in the dialog).
pub fn check_portal_stream(recorder_ready: &Value, game_ready: &Value) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let portal = &recorder_ready["portal"];
    let game_monitor = game_ready["monitor_name"].as_str().unwrap_or("?");
    let again = format!(
        "delete {} and pick only {game_monitor} in the dialog",
        portal_token().display()
    );
    let Some(pos) = portal["position"].as_array() else {
        panic!("the portal stream is not a single monitor ({portal}); {again}");
    };
    // winit reports monitor positions in physical pixels at its own scale; the portal in
    // logical pixels.
    let game = game_ready["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == game_monitor)
        .unwrap();
    let scale = game["scale"].as_f64().unwrap();
    let expect = game["position"].as_array().unwrap();
    for (p, e) in pos.iter().zip(expect) {
        let (p, e) = (p.as_f64().unwrap(), e.as_f64().unwrap());
        assert!(
            (p * scale - e).abs() <= scale + 1.0,
            "the portal stream is at {pos:?} (logical), not on {game_monitor} at {expect:?}; {again}"
        );
    }
}

pub fn game_exe() -> PathBuf {
    let exe = exe_dir().join(if cfg!(windows) {
        "klip-testgame.exe"
    } else {
        "klip-testgame"
    });
    assert!(
        exe.exists(),
        "{} missing: build it (cargo xtask e2e does)",
        exe.display()
    );
    exe
}

/// Ask the recorder to stop and wait up to 10 s for it to exit.
pub fn stop_recorder(discovery: &Path, rec: &mut KillOnDrop) {
    let _ = cli(discovery).arg("stop").status();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if rec.0.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
