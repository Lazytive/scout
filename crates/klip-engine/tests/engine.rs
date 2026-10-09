//! The engine's commands and settings handling, without recording: the replay buffer is
//! off (so a game the human is playing is never recorded by a test) and the control
//! channel is off (so a running Klip's discovery file is never touched).

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::sync::mpsc::Receiver;
use std::time::Duration;

use klip_engine::{Engine, EngineOptions, Event};
use serde_json::{Value, json};

fn quiet_settings(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("settings.json");
    let s = json!({
        "app": {"control_channel": false},
        "clips": {"folder": dir.join("clips")},
        "hotkeys": [],
        "games": {"buffer": "off"},
    });
    std::fs::write(&path, s.to_string()).unwrap();
    path
}

fn start(path: std::path::PathBuf) -> (Engine, Receiver<Event>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(klip_engine::events::QUEUE);
    let mut opts = EngineOptions::new(path);
    opts.events = Some(tx);
    opts.poll = Duration::from_millis(100);
    (Engine::start(opts).unwrap(), rx)
}

fn next(rx: &Receiver<Event>, name: &str) -> Value {
    loop {
        let e = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no {name} event"))
            .to_json();
        if e["event"] == name {
            return e;
        }
    }
}

#[test]
fn commands_and_strict_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = quiet_settings(dir.path());
    let (engine, rx) = start(path.clone());
    let ready = next(&rx, "ready");
    assert_eq!(ready["settings"], "loaded");
    assert_eq!(ready["port"], Value::Null);

    assert_eq!(engine.handle("ping", &Value::Null).unwrap()["pong"], true);
    let st = engine.handle("engine.status", &Value::Null).unwrap();
    assert_eq!(st["buffer_mode"], "off");
    assert_eq!(st["recording"], false);
    assert_eq!(st["game"], Value::Null);

    let e = engine
        .handle("clip.save", &json!({"last_ms": 5000}))
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("not_ready", "no game is running")
    );
    for cmd in ["session.toggle", "bookmark.add", "recorder.status"] {
        assert_eq!(
            engine.handle(cmd, &Value::Null).unwrap_err().code,
            "not_ready"
        );
    }
    assert_eq!(
        engine.handle("nope", &Value::Null).unwrap_err().code,
        "unknown_command"
    );

    // Strict changes: rejected ones change nothing, on disk or in memory.
    let before = std::fs::read_to_string(&path).unwrap();
    let e = engine
        .handle("settings.set", &json!({"recording": {"buffer_secs": 2}}))
        .unwrap_err();
    assert_eq!(e.code, "bad_args");
    assert!(
        e.message.starts_with("recording.buffer_secs"),
        "{}",
        e.message
    );
    let e = engine
        .handle("settings.set", &json!({"overlay": {"colour": "red"}}))
        .unwrap_err();
    assert_eq!(e.message, "overlay.colour: unknown setting");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    let r = engine
        .handle("settings.set", &json!({"recording": {"buffer_secs": 45}}))
        .unwrap();
    assert_eq!(r["settings"]["recording"]["buffer_secs"], 45);
    let changed = next(&rx, "settings_changed");
    assert_eq!(changed["settings"]["recording"]["buffer_secs"], 45);
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk["recording"]["buffer_secs"], 45);
    let got = engine.handle("settings.get", &Value::Null).unwrap();
    assert_eq!(got["settings"]["recording"]["buffer_secs"], 45);

    assert!(!engine.exit_requested());
    engine.handle("app.quit", &Value::Null).unwrap();
    assert!(engine.exit_requested());
    engine.shutdown();
}

#[test]
fn hotkeys_are_bound_from_settings_and_rebound_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = quiet_settings(dir.path());
    let (engine, rx) = start(path);
    assert_eq!(next(&rx, "hotkeys")["bound"], json!([]));
    // A combination nobody uses.
    engine
        .handle(
            "settings.set",
            &json!({"hotkeys": [{"action": "bookmark.add", "keys": "Ctrl+Alt+Shift+F22"}]}),
        )
        .unwrap();
    let h = next(&rx, "hotkeys");
    let b = &h["bound"][0];
    assert_eq!(b["id"], "bookmark.add.0");
    assert_eq!(b["keys"], "Ctrl+Alt+Shift+F22");
    if cfg!(windows) {
        assert_eq!(b["trigger"], "CTRL+ALT+SHIFT+F22");
        assert_eq!(b["error"], Value::Null, "{h}");
    }
    let st = engine.handle("engine.status", &Value::Null).unwrap();
    assert_eq!(st["hotkeys"], h["bound"]);
    engine.shutdown();
}

#[test]
fn broken_settings_files_are_repaired_and_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"app": {"control_channel": false}, "hotkeys": [], "games": {"buffer": "off"},
           "recording": {"buffer_secs": "lots"}}"#,
    )
    .unwrap();
    let (engine, rx) = start(path.clone());
    let ready = next(&rx, "ready");
    assert_eq!(ready["settings"], "repaired");
    assert_eq!(ready["issues"].as_array().unwrap().len(), 1, "{ready}");
    assert!(dir.path().join("settings.json.bad").exists());
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk["recording"]["buffer_secs"], 120);
    engine.shutdown();
}
