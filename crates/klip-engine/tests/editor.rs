//! M8.4: editor and export commands over a temp folder. The clip is a real MP4 of
//! made-up H.264 packets: the library indexes it exactly, nothing can decode it, so an
//! export ends with `export_done { ok: false }` (and leaves no file), which is what the
//! job and event path is tested for; real exports are the e2e test (M8.6).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use bytes::Bytes;
use klip_engine::events::Event;
use klip_engine::{Engine, EngineOptions};
use klip_media::{Codec, NANOS_PER_SEC, Packet, TrackInfo, TrackParams, VideoParams};
use klip_mp4::{Mp4Options, Mp4Track};
use serde_json::{Value, json};

fn settings(dir: &Path) -> PathBuf {
    let path = dir.join("settings.json");
    let s = json!({
        "app": {"control_channel": false},
        "clips": {"folder": dir.join("clips")},
        "hotkeys": [],
        "games": {"buffer": "off", "detect_stores": false},
    });
    std::fs::write(&path, s.to_string()).unwrap();
    path
}

fn start(path: PathBuf) -> (Engine, Receiver<Event>) {
    // Never touch the user's encoder cache, and probe in this process: the test binary
    // has no probe worker (every isolated probe would "fail" and be remembered).
    klip_engine::config::test_overrides(Some(path.with_file_name("encoders.json")), true);
    let (tx, rx) = std::sync::mpsc::sync_channel(klip_engine::events::QUEUE);
    let mut opts = EngineOptions::new(path);
    opts.events = Some(tx);
    opts.poll = Duration::from_millis(100);
    opts.media_worker = None;
    (Engine::start(opts).unwrap(), rx)
}

fn next(rx: &Receiver<Event>, name: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let e = rx
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("no {name} event"))
            .to_json();
        if e["event"] == name {
            return e;
        }
    }
}

/// A 60 fps clip of `frames` made-up H.264 packets (indexable, not decodable).
fn clip(path: &Path, frames: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let info = TrackInfo {
        codec: Codec::H264,
        codec_data: Bytes::from_static(&[1, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 0]),
        params: TrackParams::Video(VideoParams {
            width: 1280,
            height: 720,
            colorimetry: None,
        }),
        bitrate: None,
    };
    let frame = NANOS_PER_SEC / 60;
    let packets: Vec<Packet> = (0..frames)
        .map(|i| Packet {
            pts: i as i64 * frame,
            dts: i as i64 * frame,
            duration: frame,
            keyframe: i % 60 == 0,
            data: Bytes::from(vec![0u8; 64]),
        })
        .collect();
    let mut f = std::fs::File::create(path).unwrap();
    klip_mp4::write_mp4(
        &mut f,
        &[Mp4Track::new(&info, &packets)],
        &Mp4Options {
            creation_time_unix: 1_700_000_000,
            force_co64: false,
            markers: &[klip_mp4::Marker {
                at: NANOS_PER_SEC,
                title: "Bookmark 1".into(),
            }],
        },
    )
    .unwrap();
}

fn project_file(dir: &Path, key: &str) -> PathBuf {
    dir.join("clips")
        .join(".klip")
        .join("projects")
        .join(format!("{}.json", key.replace('/', "%2F")))
}

#[test]
fn project_get_set_round_trip_and_hostile_args() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clips").join("Game").join("Clip A.mp4");
    clip(&path, 120);
    let (engine, _rx) = start(settings(dir.path()));
    let p = path.to_string_lossy().to_string();

    let got = engine
        .handle("editor.project.get", &json!({ "path": p }))
        .unwrap();
    assert_eq!(got["status"], "new");
    assert_eq!(got["key"], "Game/Clip A.mp4");
    assert_eq!(got["source"]["duration_ms"], 2000);
    assert_eq!(got["source"]["width"], 1280);
    assert_eq!(got["source"]["bookmarks_ms"], json!([1000]));
    assert_eq!(got["project"]["source_ms"], 2000);
    assert!(got["issues"].as_array().unwrap().is_empty());
    assert!(!project_file(dir.path(), "Game/Clip A.mp4").exists());

    let mut project = got["project"].clone();
    project["pieces"] = json!([{ "start_ms": 500, "end_ms": 1500, "speed": 2.0 }]);
    project["text"] = json!([{ "id": 1, "preset": "pill", "text": " gg ", "position": "bottom", "start_ms": 0, "end_ms": 400 }]);
    project["volumes"] = json!({ "game": 0.5, "mic": 7.0 });
    project["bogus"] = json!({ "advanced": true });
    let set = engine
        .handle(
            "editor.project.set",
            &json!({ "path": p, "project": project }),
        )
        .unwrap();
    assert_eq!(set["status"], "saved");
    assert_eq!(set["project"]["pieces"][0]["speed"], 2.0);
    assert_eq!(set["project"]["text"][0]["text"], "gg");
    assert_eq!(set["project"]["volumes"]["mic"], 2.0, "clamped");
    assert!(
        !set["issues"].as_array().unwrap().is_empty(),
        "the clamp is reported"
    );
    assert!(project_file(dir.path(), "Game/Clip A.mp4").is_file());

    let again = engine
        .handle("editor.project.get", &json!({ "path": p }))
        .unwrap();
    assert_eq!(again["status"], "saved");
    assert_eq!(again["project"], set["project"]);
    assert_eq!(
        again["project"]["bogus"]["advanced"], true,
        "unknown keys survive"
    );

    // Hostile arguments.
    for args in [
        json!(null),
        json!({ "path": 5 }),
        json!({ "path": dir.path().join("outside.mp4").to_string_lossy() }),
        json!({ "path": dir.path().join("clips").join("missing.mp4").to_string_lossy() }),
        json!({ "path": "../../etc/passwd" }),
    ] {
        assert_eq!(
            engine.handle("editor.project.get", &args).unwrap_err().code,
            "bad_args",
            "{args}"
        );
    }
    for args in [
        json!({ "path": p }),
        json!({ "path": p, "project": [] }),
        json!({ "path": p, "project": "x" }),
    ] {
        assert_eq!(
            engine.handle("editor.project.set", &args).unwrap_err().code,
            "bad_args",
            "{args}"
        );
    }
    let huge = json!({ "path": p, "project": { "pieces": [], "pad": "x".repeat(2_000_000) } });
    assert_eq!(
        engine.handle("editor.project.set", &huge).unwrap_err().code,
        "bad_args"
    );
    // Nonsense inside the project is repaired, not refused.
    let junk = json!({ "path": p, "project": { "pieces": "no", "source_ms": -4, "text": [{ "preset": 9 }] } });
    let r = engine.handle("editor.project.set", &junk).unwrap();
    assert_eq!(r["project"]["source_ms"], 2000);
    assert!(r["project"]["pieces"].as_array().unwrap().is_empty());
}

#[test]
fn rename_keeps_the_project_and_delete_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clips").join("a.mp4");
    clip(&path, 60);
    let (engine, rx) = start(settings(dir.path()));
    let p = path.to_string_lossy().to_string();
    let mut project = engine
        .handle("editor.project.get", &json!({ "path": p }))
        .unwrap()["project"]
        .clone();
    project["pieces"] = json!([{ "start_ms": 0, "end_ms": 500, "speed": 1.0 }]);
    engine
        .handle(
            "editor.project.set",
            &json!({ "path": p, "project": project }),
        )
        .unwrap();
    assert!(project_file(dir.path(), "a.mp4").is_file());

    let renamed = engine
        .handle("clip.rename", &json!({ "path": p, "name": "Better" }))
        .unwrap();
    next(&rx, "library_changed");
    assert_eq!(renamed["key"], "Better.mp4");
    assert!(!project_file(dir.path(), "a.mp4").exists());
    assert!(project_file(dir.path(), "Better.mp4").is_file());
    let got = engine
        .handle("editor.project.get", &json!({ "path": renamed["path"] }))
        .unwrap();
    assert_eq!(got["status"], "saved");
    assert_eq!(got["project"]["pieces"][0]["end_ms"], 500);

    engine
        .handle(
            "clip.delete",
            &json!({ "paths": [renamed["path"]], "permanent": true }),
        )
        .unwrap();
    assert!(!project_file(dir.path(), "Better.mp4").exists());
    assert!(!dir.path().join("clips").join("Better.mp4").exists());
}

#[test]
fn proxy_of_an_h264_clip_is_the_clip_itself() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clips").join("a.mp4");
    clip(&path, 60);
    let (engine, _rx) = start(settings(dir.path()));
    let r = engine
        .handle("editor.proxy", &json!({ "path": path.to_string_lossy() }))
        .unwrap();
    assert_eq!(r["status"], "original");
    assert_eq!(r["path"], json!(path.to_string_lossy()));
    assert_eq!(
        engine
            .handle("editor.proxy", &json!({ "path": "nope" }))
            .unwrap_err()
            .code,
        "bad_args"
    );
}

#[test]
fn text_layers_render_for_the_preview() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clips").join("a.mp4");
    clip(&path, 60);
    let (engine, _rx) = start(settings(dir.path()));
    let p = path.to_string_lossy().to_string();
    let layer = json!({ "id": 1, "preset": "pill", "text": "gg wp", "position": "bottom", "start_ms": 0, "end_ms": 1000 });
    let r = engine
        .handle(
            "editor.text.render",
            &json!({ "path": p, "layer": layer, "width": 640, "height": 360 }),
        )
        .unwrap();
    assert_eq!(
        (r["frame_width"].as_u64(), r["frame_height"].as_u64()),
        (Some(640), Some(360))
    );
    assert!(r["width"].as_u64().unwrap() > 0 && r["height"].as_u64().unwrap() > 0);
    assert!(r["png"].as_str().unwrap().len() > 100);
    for args in [
        json!({ "path": p }),
        json!({ "path": p, "layer": "x", "width": 640, "height": 360 }),
        json!({ "path": p, "layer": layer, "width": 0, "height": 360 }),
        json!({ "path": p, "layer": layer, "width": 640, "height": 5000 }),
        json!({ "path": "nope", "layer": layer, "width": 640, "height": 360 }),
    ] {
        assert_eq!(
            engine.handle("editor.text.render", &args).unwrap_err().code,
            "bad_args",
            "{args}"
        );
    }
}

#[test]
fn export_commands_run_a_job_and_report_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clips").join("Game").join("Clip.mp4");
    clip(&path, 120);
    let (engine, rx) = start(settings(dir.path()));
    let p = path.to_string_lossy().to_string();

    for args in [
        json!(null),
        json!({ "path": p }),
        json!({ "path": p, "preset": "Discord" }),
        json!({ "path": p, "preset": "proxy" }),
    ] {
        assert_eq!(
            engine.handle("export.start", &args).unwrap_err().code,
            "bad_args",
            "{args}"
        );
    }
    assert_eq!(
        engine
            .handle("export.cancel", &json!({ "id": 7 }))
            .unwrap_err()
            .code,
        "not_found"
    );
    let idle = engine.handle("export.status", &json!(null)).unwrap();
    assert!(idle["active"].is_null() && idle["last"].is_null());

    let started = match engine.handle("export.start", &json!({ "path": p, "preset": "discord" })) {
        Ok(v) => v,
        Err(e) if e.code == "failed" && e.message.contains("encoder") => {
            eprintln!("skipped: {}", e.message);
            return;
        }
        Err(e) => panic!("{e:?}"),
    };
    let id = started["id"].as_u64().unwrap();
    let out = PathBuf::from(started["out"].as_str().unwrap());
    assert_eq!(out, path.with_file_name("Clip - Discord.mp4"));
    // While it runs: status says so and a second one is refused.
    let busy = engine.handle("export.start", &json!({ "path": p, "preset": "youtube" }));
    let status = engine.handle("export.status", &json!(null)).unwrap();
    if status["active"].is_object() {
        assert_eq!(status["active"]["id"], id);
        assert_eq!(busy.unwrap_err().code, "busy");
    }
    let done = next(&rx, "export_done");
    assert_eq!(done["id"], id);
    assert_eq!(done["key"], "Game/Clip.mp4");
    assert_eq!(done["preset"], "discord");
    // The made-up packets can't be decoded: the job fails cleanly and leaves nothing.
    assert_eq!(done["ok"], false, "{done}");
    assert!(done["error"].is_string());
    assert!(!out.exists());
    assert!(!out.with_file_name("Clip - Discord.mp4.part").exists());
    let after = engine.handle("export.status", &json!(null)).unwrap();
    assert!(after["active"].is_null());
    assert_eq!(after["last"]["id"], id);
    assert_eq!(after["last"]["ok"], false);
    assert_eq!(
        engine
            .handle("export.cancel", &json!({ "id": id }))
            .unwrap_err()
            .code,
        "not_found"
    );
    // The clip list is unchanged (no half file was indexed).
    let list = engine.handle("library.list", &json!(null)).unwrap();
    assert_eq!(list["clips"].as_array().unwrap().len(), 1);
}

#[test]
fn discord_limit_is_a_setting() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _rx) = start(settings(dir.path()));
    let s = engine.handle("settings.get", &json!(null)).unwrap();
    assert_eq!(s["settings"]["export"]["discord_limit_mb"], 10);
    engine
        .handle(
            "settings.set",
            &json!({ "export": { "discord_limit_mb": 25 } }),
        )
        .unwrap();
    let s = engine.handle("settings.get", &json!(null)).unwrap();
    assert_eq!(s["settings"]["export"]["discord_limit_mb"], 25);
    assert!(
        engine
            .handle(
                "settings.set",
                &json!({ "export": { "discord_limit_mb": 0 } })
            )
            .is_err()
    );
}
