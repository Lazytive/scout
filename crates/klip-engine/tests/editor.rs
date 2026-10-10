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

fn history_dir(dir: &Path, key: &str) -> PathBuf {
    dir.join("clips")
        .join(".klip")
        .join("history")
        .join(key.replace('/', "%2F"))
}

/// M9.4: a montage under its first clip; a clip that goes away is reported and kept,
/// and blocks the export by name; a renamed clip is followed; project history.
#[test]
fn montage_missing_clips_history_and_rename() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("clips").join("Game");
    let (a, b, c) = (game.join("A.mp4"), game.join("B.mp4"), game.join("C.mp4"));
    clip(&a, 120);
    clip(&b, 180);
    clip(&c, 60);
    let (engine, rx) = start(settings(dir.path()));
    let s = |p: &Path| p.to_string_lossy().to_string();

    // Hostile arguments.
    for args in [
        json!({ "path": s(&a) }),
        json!({ "path": s(&a), "add": [] }),
        json!({ "path": s(&a), "add": "B.mp4" }),
        json!({ "path": s(&a), "add": [7] }),
        json!({ "path": s(&a), "add": [dir.path().join("elsewhere.mp4")] }),
        json!({ "path": s(&a), "add": [s(&game.join("none.mp4"))] }),
        json!({ "path": "..", "add": [s(&b)] }),
    ] {
        assert_eq!(
            engine.handle("editor.montage", &args).unwrap_err().code,
            "bad_args",
            "{args}"
        );
    }
    assert!(!project_file(dir.path(), "Game/A.mp4").exists());

    // A + B + C: the clips follow A's 2 s, whole, on a new montage track.
    let m = engine
        .handle(
            "editor.montage",
            &json!({ "path": s(&a), "add": [s(&b), s(&c)] }),
        )
        .unwrap();
    assert_eq!(m["key"], "Game/A.mp4");
    let track = &m["project"]["advanced"]["tracks"][0];
    assert_eq!(track["name"], "Montage");
    let placed: Vec<(String, u64, u64)> = track["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["source"]["key"].as_str().unwrap_or("").to_string(),
                c["at_ms"].as_u64().unwrap(),
                c["out_ms"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        placed,
        vec![
            ("Game/B.mp4".to_string(), 2_000, 3_000),
            ("Game/C.mp4".to_string(), 5_000, 1_000)
        ]
    );
    let clips = m["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 2);
    assert_eq!(
        (&clips[0]["status"], &clips[0]["name"]),
        (&json!("ok"), &json!("B.mp4"))
    );
    assert_eq!(clips[0]["source"]["duration_ms"], 3_000);
    assert!(project_file(dir.path(), "Game/A.mp4").is_file());
    assert!(
        !history_dir(dir.path(), "Game/A.mp4").exists(),
        "nothing replaced yet"
    );

    // A Custom export of the montage is a job (the made-up clips can't decode).
    for bad in [
        json!({ "path": s(&a), "preset": "custom" }),
        json!({ "path": s(&a), "preset": "custom", "custom": { "width": 1280, "height": 720, "fps": 30, "bitrate_kbps": 6000, "codec": "x264" } }),
    ] {
        assert_eq!(
            engine.handle("export.start", &bad).unwrap_err().code,
            "bad_args"
        );
    }
    let custom = json!({ "path": s(&a), "preset": "custom", "custom": { "width": 640, "height": 360, "fps": 30, "bitrate_kbps": 2000, "codec": "h264" } });
    match engine.handle("export.start", &custom) {
        Ok(started) => {
            assert!(
                started["out"].as_str().unwrap().ends_with("A - Custom.mp4"),
                "{started}"
            );
            assert_eq!(started["preset"], "custom");
            let done = next(&rx, "export_done");
            assert_eq!(
                (&done["ok"], &done["preset"]),
                (&json!(false), &json!("custom"))
            );
        }
        Err(e) if e.code == "failed" && e.message.contains("encoder") => {
            eprintln!("custom export skipped: {}", e.message);
        }
        Err(e) => panic!("{e:?}"),
    }

    // History: the next real change keeps the montage as a snapshot.
    let mut changed = m["project"].clone();
    changed["advanced"]["markers"] = json!([{ "at_ms": 100, "title": "Start", "kind": "user" }]);
    engine
        .handle(
            "editor.project.set",
            &json!({ "path": s(&a), "project": changed }),
        )
        .unwrap();
    let list = engine
        .handle("editor.history.list", &json!({ "path": s(&a) }))
        .unwrap();
    let snaps = list["snapshots"].as_array().unwrap();
    assert_eq!(snaps.len(), 1);
    let id = snaps[0]["id"].as_u64().unwrap();
    for args in [
        json!({ "path": s(&a) }),
        json!({ "path": s(&a), "id": "1" }),
        json!({ "path": s(&a), "id": -1 }),
    ] {
        assert_eq!(
            engine
                .handle("editor.history.restore", &args)
                .unwrap_err()
                .code,
            "bad_args",
            "{args}"
        );
    }
    assert_eq!(
        engine
            .handle(
                "editor.history.restore",
                &json!({ "path": s(&a), "id": id + 1 })
            )
            .unwrap_err()
            .code,
        "not_found"
    );
    let restored = engine
        .handle(
            "editor.history.restore",
            &json!({ "path": s(&a), "id": id }),
        )
        .unwrap();
    assert_eq!(restored["status"], "restored");
    assert!(
        restored["project"]["advanced"]["markers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let after = engine
        .handle("editor.history.list", &json!({ "path": s(&a) }))
        .unwrap();
    assert_eq!(
        after["snapshots"].as_array().unwrap().len(),
        2,
        "the replaced project is a snapshot too"
    );

    // C is renamed: the montage follows it.
    let renamed = engine
        .handle("clip.rename", &json!({ "path": s(&c), "name": "Clutch" }))
        .unwrap();
    assert_eq!(renamed["key"], "Game/Clutch.mp4");
    let got = engine
        .handle("editor.project.get", &json!({ "path": s(&a) }))
        .unwrap();
    assert_eq!(
        got["project"]["advanced"]["tracks"][0]["clips"][1]["source"]["key"],
        "Game/Clutch.mp4"
    );
    assert_eq!(got["clips"][1]["status"], "ok");

    // B is deleted: reported missing, never dropped, and the export names it.
    engine
        .handle(
            "clip.delete",
            &json!({ "paths": [s(&b)], "permanent": true }),
        )
        .unwrap();
    let got = engine
        .handle("editor.project.get", &json!({ "path": s(&a) }))
        .unwrap();
    assert_eq!(
        (&got["clips"][0]["key"], &got["clips"][0]["status"]),
        (&json!("Game/B.mp4"), &json!("missing"))
    );
    assert!(got["clips"][0]["source"].is_null());
    assert_eq!(
        got["project"]["advanced"]["tracks"][0]["clips"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let e = engine
        .handle(
            "export.start",
            &json!({ "path": s(&a), "preset": "youtube" }),
        )
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("missing", "missing from the clip folder: B.mp4")
    );
    // A frame where B would show says so; the frame args are checked first.
    let e = engine
        .handle(
            "editor.frame",
            &json!({ "path": s(&a), "out_ms": 2_500, "width": 320 }),
        )
        .unwrap_err();
    assert_eq!(e.code, "missing");
    for args in [
        json!({ "path": s(&a), "width": 320 }),
        json!({ "path": s(&a), "out_ms": -1, "width": 320 }),
        json!({ "path": s(&a), "out_ms": 0, "width": 8 }),
        json!({ "path": s(&a), "out_ms": 0, "width": 5000 }),
        json!({ "path": s(&a), "out_ms": 0, "width": 320, "project": [] }),
        json!({ "path": "nope", "out_ms": 0, "width": 320 }),
    ] {
        assert_eq!(
            engine.handle("editor.frame", &args).unwrap_err().code,
            "bad_args",
            "{args}"
        );
    }
    // Deleting the montage's own clip takes its project and history with it.
    engine
        .handle(
            "clip.delete",
            &json!({ "paths": [s(&a)], "permanent": true }),
        )
        .unwrap();
    assert!(!project_file(dir.path(), "Game/A.mp4").exists());
    assert!(!history_dir(dir.path(), "Game/A.mp4").exists());
}

/// A playable H.264 clip of `frames` 60 fps frames at 640x360, made with the OS (or
/// Cisco's) encoder.
fn real_clip(path: &Path, frames: u32, pattern: &str) -> bool {
    use gstreamer::prelude::*;
    gstreamer::init().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let Some(enc) = ["mfh264enc", "openh264enc"]
        .into_iter()
        .find(|e| gstreamer::ElementFactory::find(e).is_some())
    else {
        return false;
    };
    let line = format!(
        "videotestsrc num-buffers={frames} pattern={pattern} ! video/x-raw,format=NV12,width=640,height=360,framerate=60/1 ! {enc} ! h264parse ! mp4mux ! filesink location=\"{}\"",
        path.to_string_lossy().replace('\\', "/")
    );
    let pipeline = gstreamer::parse::launch(&line).unwrap();
    pipeline.set_state(gstreamer::State::Playing).unwrap();
    let bus = pipeline.bus().unwrap();
    let ok = bus
        .timed_pop_filtered(
            gstreamer::ClockTime::from_seconds(30),
            &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
        )
        .is_some_and(|m| m.type_() == gstreamer::MessageType::Eos);
    pipeline.set_state(gstreamer::State::Null).unwrap();
    ok
}

/// M9.4: `editor.frame` draws the montage through the export's renderer (in this
/// process here; the app runs it in the media worker).
#[test]
#[ignore = "hardware: encodes and decodes real clips"]
fn frames_of_a_montage() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("clips").join("Game");
    let (a, b) = (game.join("A.mp4"), game.join("B.mp4"));
    if !real_clip(&a, 120, "smpte") || !real_clip(&b, 120, "ball") {
        eprintln!("skipped: no H.264 encoder to make the clips");
        return;
    }
    let (engine, _rx) = start(settings(dir.path()));
    let s = |p: &Path| p.to_string_lossy().to_string();
    engine
        .handle("editor.montage", &json!({ "path": s(&a), "add": [s(&b)] }))
        .unwrap();
    let frame = |out_ms: u64, width: u32, project: Value| {
        let t = Instant::now();
        let f = engine
            .handle(
                "editor.frame",
                &json!({ "path": s(&a), "out_ms": out_ms, "width": width, "project": project }),
            )
            .unwrap();
        println!(
            "frame at {out_ms} ms, {width} wide: {:.0} ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
        f
    };
    use base64::Engine as _;
    let jpeg = |f: &Value| {
        base64::engine::general_purpose::STANDARD
            .decode(f["jpeg"].as_str().unwrap())
            .unwrap()
    };
    let first = frame(500, 320, Value::Null);
    assert_eq!(
        (&first["width"], &first["height"], &first["out_ms"]),
        (&json!(320), &json!(180), &json!(500))
    );
    let a_jpeg = jpeg(&first);
    assert_eq!(&a_jpeg[..2], &[0xFF, 0xD8], "a JPEG");
    let second = frame(2_500, 320, Value::Null);
    assert_ne!(jpeg(&second), a_jpeg, "B shows from 2 s");
    let last = frame(u64::MAX, 640, Value::Null);
    assert_eq!(
        (&last["width"], &last["height"]),
        (&json!(640), &json!(360))
    );
    assert!(last["out_ms"].as_u64().unwrap() >= 3_990);
    // An unsaved project from the UI: a tall crop changes the frame's shape.
    let mut p = engine
        .handle("editor.project.get", &json!({ "path": s(&a) }))
        .unwrap()["project"]
        .clone();
    p["format"]["kind"] = json!("tall");
    let tall = frame(500, 180, p);
    assert_eq!(tall["width"], 180);
    let h = tall["height"].as_u64().unwrap();
    assert!((318..=322).contains(&h), "9:16 at 180 wide: {h}");
}
