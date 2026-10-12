//! The engine's commands and settings handling, without recording: the replay buffer is
//! off (so a game the human is playing is never recorded by a test) and the control
//! channel is off (so a running Scout's discovery file is never touched).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

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

/// A clip of `mib` MiB without writing the bytes (the length is what counts).
fn big_clip(path: &std::path::Path, mib: u64, age_secs: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let f = std::fs::File::create(path).unwrap();
    f.set_len(mib << 20).unwrap();
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(age_secs))
        .unwrap();
}

#[test]
fn storage_cap_removes_the_oldest_unstarred_clips() {
    let dir = tempfile::tempdir().unwrap();
    if klip_platform::free_disk_space(dir.path()).unwrap() < 4 << 30 {
        eprintln!("skipped: needs 4 GiB free for the test files");
        return;
    }
    let clips = dir.path().join("clips");
    let starred_old = clips.join("G/starred-old.mp4");
    let old = clips.join("G/old.mp4");
    let mid = clips.join("H/mid.mp4");
    let new = clips.join("G/new.mp4");
    big_clip(&starred_old, 400, 900);
    big_clip(&old, 400, 600);
    big_clip(&mid, 400, 300);
    big_clip(&new, 100, 10);
    std::fs::create_dir_all(clips.join(".klip")).unwrap();
    std::fs::write(
        clips.join(".klip/library.json"),
        r#"{"version":1,"clips":{"G/starred-old.mp4":{"starred":true}}}"#,
    )
    .unwrap();
    let path = dir.path().join("settings.json");
    let s = json!({
        "app": {"control_channel": false},
        "clips": {"folder": clips},
        "hotkeys": [],
        "games": {"buffer": "off"},
        "storage": {"cap_gb": 1, "over_cap": "delete", "low_disk_gb": 100000},
    });
    std::fs::write(&path, s.to_string()).unwrap();
    // 1300 MiB against a 1 GiB cap: the oldest unstarred clip goes at start.
    let (engine, rx) = start(path);
    let cleaned = next(&rx, "storage_cleaned");
    assert_eq!(cleaned["report"]["removed"], 1, "{cleaned}");
    assert_eq!(cleaned["report"]["to_trash"], false);
    assert!(starred_old.exists() && !old.exists() && mid.exists() && new.exists());

    let st = engine.handle("storage.status", &Value::Null).unwrap();
    assert_eq!(st["usage"]["clips"], 3);
    assert_eq!(st["usage"]["starred"], 1);
    assert_eq!(st["cap_bytes"], 1u64 << 30);
    assert_eq!(st["low_disk"], true, "the threshold is 100 TB");
    let w = next(&rx, "warning");
    assert!(
        w["message"].as_str().unwrap().starts_with("Low disk space"),
        "{w}"
    );

    // Unstarring doesn't remove anything while under the cap; stars need a real clip.
    engine
        .handle("clip.star", &json!({"path": starred_old, "starred": false}))
        .unwrap();
    let r = engine.handle("storage.cleanup", &Value::Null).unwrap();
    assert_eq!(r["removed"], 0);
    let e = engine
        .handle("clip.star", &json!({"path": old, "starred": true}))
        .unwrap_err();
    assert_eq!(e.code, "bad_args");
    engine.shutdown();
}

/// Regression (IMPROVEMENTS B2): a clean-up that isn't a save's own (at start, a
/// changed cap, `storage.cleanup`) used to protect nothing, so the clip just saved went
/// when it alone was over the cap.
#[test]
fn storage_cap_never_removes_the_newest_clip() {
    let dir = tempfile::tempdir().unwrap();
    if klip_platform::free_disk_space(dir.path()).unwrap() < 3 << 30 {
        eprintln!("skipped: needs 3 GiB free for the test files");
        return;
    }
    let clips = dir.path().join("clips");
    let old = clips.join("G/old.mp4");
    let new = clips.join("G/new.mp4");
    big_clip(&old, 300, 600);
    big_clip(&new, 1200, 10);
    let path = dir.path().join("settings.json");
    let s = json!({
        "app": {"control_channel": false},
        "clips": {"folder": clips},
        "hotkeys": [],
        "games": {"buffer": "off"},
        "storage": {"cap_gb": 1, "over_cap": "delete", "low_disk_gb": 0},
    });
    std::fs::write(&path, s.to_string()).unwrap();
    let (engine, rx) = start(path);
    let cleaned = next(&rx, "storage_cleaned");
    assert_eq!(cleaned["report"]["removed"], 1, "{cleaned}");
    assert!(!old.exists() && new.exists());
    let r = engine.handle("storage.cleanup", &Value::Null).unwrap();
    assert_eq!(r["removed"], 0, "{r}");
    assert!(new.exists());
    engine.shutdown();
}

/// Settings changes from many threads at once: none is lost, and the file matches what
/// the engine holds (IMPROVEMENTS B3 moved the save out of the readers' lock).
#[test]
fn concurrent_settings_changes_are_all_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = quiet_settings(dir.path());
    let (engine, _rx) = start(path.clone());
    let engine = std::sync::Arc::new(engine);
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let e = engine.clone();
            std::thread::spawn(move || {
                for i in 0..10u32 {
                    let patch = match t {
                        0 => json!({"storage": {"cap_gb": 10 + i}}),
                        1 => json!({"storage": {"low_disk_gb": 20 + i}}),
                        2 => json!({"recording": {"buffer_secs": 30 + i}}),
                        _ => json!({"app": {"start_minimized": i % 2 == 0}}),
                    };
                    e.handle("settings.set", &patch).unwrap();
                    let _ = e.handle("engine.status", &Value::Null).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let got = engine.handle("settings.get", &Value::Null).unwrap();
    let s = &got["settings"];
    assert_eq!(s["storage"]["cap_gb"], 19, "{s}");
    assert_eq!(s["storage"]["low_disk_gb"], 29);
    assert_eq!(s["recording"]["buffer_secs"], 39);
    assert_eq!(s["app"]["start_minimized"], false);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(&file, s);
    match std::sync::Arc::try_unwrap(engine) {
        Ok(e) => e.shutdown(),
        Err(_) => panic!("engine still shared"),
    }
}

#[test]
fn setup_commands() {
    let dir = tempfile::tempdir().unwrap();
    let path = quiet_settings(dir.path());
    let (engine, rx) = start(path);
    next(&rx, "ready");

    // RAM estimate: from the current settings or a draft; scales with the buffer.
    let now = engine.handle("recording.estimate", &Value::Null).unwrap();
    assert_eq!(now["video_kbps"], 20_000, "{now}");
    assert_eq!(now["buffer_secs"], 120);
    let half = engine
        .handle(
            "recording.estimate",
            &json!({"recording": {"buffer_secs": 60}}),
        )
        .unwrap();
    let (a, b) = (
        now["bytes"].as_u64().unwrap(),
        half["bytes"].as_u64().unwrap(),
    );
    assert!(b < a && b * 2 > a, "{a} {b}");
    let low = engine
        .handle(
            "recording.estimate",
            &json!({"recording": {"quality": {"preset": "low"}}, "audio": {"desktop": "off", "voice": false}}),
        )
        .unwrap();
    assert_eq!(
        (low["video_kbps"].as_u64(), low["audio_tracks"].as_u64()),
        (Some(8_000), Some(0))
    );

    // Hotkeys pause and come back.
    engine
        .handle(
            "settings.set",
            &json!({"hotkeys": [{"action": "bookmark.add", "keys": "Ctrl+Alt+Shift+F21"}]}),
        )
        .unwrap();
    next(&rx, "hotkeys");
    let p = engine.handle("hotkeys.pause", &Value::Null).unwrap();
    assert_eq!(p["paused"], true);
    let st = engine.handle("engine.status", &Value::Null).unwrap();
    assert_eq!(st["hotkeys_paused"], true);
    engine.handle("hotkeys.resume", &Value::Null).unwrap();
    let h = next(&rx, "hotkeys");
    assert_eq!(h["bound"][0]["keys"], "Ctrl+Alt+Shift+F21");
    let st = engine.handle("engine.status", &Value::Null).unwrap();
    assert_eq!(st["hotkeys_paused"], false);

    // Programs to add as games; audio devices; meter arguments.
    let c = engine.handle("games.candidates", &Value::Null).unwrap();
    assert!(c["programs"].is_array());
    let d = engine.handle("audio.devices", &Value::Null).unwrap();
    assert!(d["inputs"].is_array() && d["outputs"].is_array());
    assert_eq!(
        engine
            .handle("audio.meter", &json!({"on": "yes"}))
            .unwrap_err()
            .code,
        "bad_args"
    );
    assert_eq!(
        engine.handle("audio.meter", &json!({"on": false})).unwrap()["on"],
        false
    );
    engine.shutdown();
}

/// Live microphone meter on the default input (hardware: needs an audio input).
#[test]
#[ignore = "hardware: opens the default microphone"]
fn mic_meter_reports_levels() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, rx) = start(quiet_settings(dir.path()));
    next(&rx, "ready");
    engine.handle("audio.meter", &json!({"on": true})).unwrap();
    let mut n = 0;
    while n < 5 {
        let e = next(&rx, "mic_level");
        assert!(e["peak_db"].as_f64().unwrap() <= 0.0);
        n += 1;
    }
    engine.handle("audio.meter", &json!({"on": false})).unwrap();
    engine.shutdown();
}
