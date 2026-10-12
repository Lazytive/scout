//! The library commands on a temp clip folder (no recording, no control channel, nothing
//! moved to the real Recycle Bin: deletes here are `permanent`). The clips are MP4s with
//! made-up packets: their info reads back exactly, but they don't decode, which tests
//! the thumbnail failure path (made in-process here; the child-process worker and real
//! pictures are covered by `apps/klip-cli/tests/m7_library.rs`).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use bytes::Bytes;
use klip_engine::{Engine, EngineOptions, Event};
use klip_media::{Codec, NANOS_PER_SEC, Packet, TrackInfo, TrackParams, VideoParams};
use klip_mp4::{Mp4Options, Mp4Track};
use serde_json::{Value, json};

fn settings(dir: &Path) -> PathBuf {
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

fn start(path: PathBuf) -> (Engine, Receiver<Event>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(klip_engine::events::QUEUE);
    let mut opts = EngineOptions::new(path);
    opts.events = Some(tx);
    opts.poll = Duration::from_millis(100);
    opts.media_worker = None;
    (Engine::start(opts).unwrap(), rx)
}

fn next(rx: &Receiver<Event>, name: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
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

/// A 60 fps clip of `frames` made-up H.264 packets.
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
    klip_mp4::save_mp4_atomic(
        path,
        &[Mp4Track::new(&info, &packets)],
        &Mp4Options::default(),
    )
    .unwrap();
}

fn list(engine: &Engine, q: Value) -> Value {
    engine.handle("library.list", &q).unwrap()
}

fn names(l: &Value) -> Vec<String> {
    l["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn library_commands() {
    let dir = tempfile::tempdir().unwrap();
    let clips = dir.path().join("clips");
    let one = clips.join("Apex").join("Klip_1.mp4");
    let two = clips.join("Apex").join("Klip_2.mp4");
    clip(&one, 120);
    clip(&two, 600);
    clip(&clips.join("loose.mp4"), 60);
    std::fs::write(clips.join("broken.mp4"), b"not a video").unwrap();
    let (engine, rx) = start(settings(dir.path()));
    next(&rx, "ready");

    let l = list(&engine, Value::Null);
    assert_eq!(l["total"], 4);
    assert_eq!(l["strip"]["tiles"], 16);
    assert_eq!(l["folder"], clips.to_string_lossy().as_ref());
    let by_name = |l: &Value, n: &str| {
        l["clips"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == n)
            .cloned()
            .unwrap()
    };
    assert_eq!(by_name(&l, "Klip_2")["info"]["duration_ms"], 10_000);
    assert_eq!(by_name(&l, "Klip_2")["game"], "Apex");
    assert_eq!(by_name(&l, "broken")["info"], Value::Null);
    assert!(by_name(&l, "broken")["error"].is_string());
    let games: Vec<(String, u64)> = l["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["name"].as_str().unwrap().into(),
                g["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(games, vec![(String::new(), 2), ("Apex".into(), 2)]);
    assert_eq!(
        names(&list(&engine, json!({"game": "Apex", "sort": "longest"}))),
        ["Klip_2", "Klip_1"]
    );
    assert_eq!(
        engine
            .handle("library.list", &json!({"bogus": 1}))
            .unwrap_err()
            .code,
        "bad_args"
    );
    assert!(dir.path().join("clips/.klip/index.json").exists());

    // None of these decode: every clip's thumbnails fail once and are not retried.
    let t = engine
        .handle("library.thumbs", &json!({"keys": ["Apex/Klip_1.mp4"]}))
        .unwrap();
    assert!(t["queued"].as_u64().unwrap() <= 1);
    let mut failed = std::collections::HashSet::new();
    while failed.len() < 4 {
        let e = next(&rx, "thumbs_ready");
        assert_eq!(e["failed"], true, "{e}");
        failed.insert(e["key"].as_str().unwrap().to_string());
    }
    assert_eq!(list(&engine, Value::Null)["thumbs_pending"], 0);
    for (args, code) in [
        (json!({"keys": ["../x.mp4"]}), "bad_args"),
        (json!({"keys": "x"}), "bad_args"),
    ] {
        assert_eq!(
            engine.handle("library.thumbs", &args).unwrap_err().code,
            code
        );
    }
    let no_thumb = engine
        .handle(
            "library.thumb",
            &json!({"key": "Apex/Klip_1.mp4", "kind": "poster"}),
        )
        .unwrap_err();
    assert_eq!(no_thumb.code, "not_found");
    assert_eq!(
        engine
            .handle("library.thumb", &json!({"key": "a.mp4", "kind": "big"}))
            .unwrap_err()
            .code,
        "bad_args"
    );

    // Star, tags, rename: the clip keeps its star, tags and info.
    let p1 = one.to_string_lossy().to_string();
    engine
        .handle("clip.star", &json!({"path": p1, "starred": true}))
        .unwrap();
    assert_eq!(next(&rx, "library_changed")["reason"], "starred");
    let tags = engine
        .handle(
            "clip.tags",
            &json!({"path": p1, "tags": ["ace", "Ace", " ranked "]}),
        )
        .unwrap();
    assert_eq!(tags["tags"], json!(["ace", "ranked"]));
    let r = engine
        .handle("clip.rename", &json!({"path": p1, "name": "Ace round"}))
        .unwrap();
    assert_eq!(r["key"], "Apex/Ace round.mp4");
    assert_eq!(next(&rx, "library_changed")["reason"], "tags");
    assert!(!one.exists());
    let renamed = PathBuf::from(r["path"].as_str().unwrap());
    let l = list(&engine, json!({"search": "ace RANKED"}));
    assert_eq!(names(&l), ["Ace round"]);
    let c = &l["clips"][0];
    assert_eq!(
        (c["starred"].clone(), c["tags"].clone()),
        (json!(true), json!(["ace", "ranked"]))
    );
    assert_eq!(c["info"]["duration_ms"], 2000);
    for (args, why) in [
        (json!({"path": renamed, "name": "Klip_2"}), "exists"),
        (json!({"path": renamed, "name": "a/b"}), "bad name"),
        (
            json!({"path": "C:/elsewhere/x.mp4", "name": "y"}),
            "outside",
        ),
        (json!({"path": renamed}), "no name"),
    ] {
        assert_eq!(
            engine.handle("clip.rename", &args).unwrap_err().code,
            "bad_args",
            "{why}"
        );
    }
    assert_eq!(
        engine
            .handle("clip.tags", &json!({"path": renamed, "tags": [1]}))
            .unwrap_err()
            .code,
        "bad_args"
    );
    assert_eq!(
        engine
            .handle("clip.open", &json!({"path": "C:/elsewhere/x.mp4"}))
            .unwrap_err()
            .code,
        "bad_args"
    );
    // Copy refuses before touching the clipboard (the happy path is in e2e below).
    for bad in [
        json!({"paths": "x"}),
        json!({"paths": []}),
        json!({"paths": [1]}),
        json!({"paths": ["C:/elsewhere/x.mp4"]}),
    ] {
        assert_eq!(
            engine.handle("clip.copy", &bad).unwrap_err().code,
            "bad_args",
            "{bad}"
        );
    }

    // Delete (permanently here; the app's default is the Recycle Bin).
    let d = engine
        .handle(
            "clip.delete",
            &json!({"paths": [renamed, "C:/elsewhere/x.mp4"], "permanent": true}),
        )
        .unwrap();
    assert_eq!(
        (d["removed"].clone(), d["to_trash"].clone()),
        (json!(1), json!(false))
    );
    assert_eq!(d["failed"].as_array().unwrap().len(), 1);
    assert!(!renamed.exists() && two.exists());
    let l = list(&engine, Value::Null);
    assert_eq!(l["total"], 3);
    assert_eq!(l["starred"], 0);
    let meta = std::fs::read_to_string(clips.join(".klip/library.json")).unwrap();
    assert!(!meta.contains("Ace round"), "{meta}");
    assert_eq!(
        engine
            .handle("clip.delete", &json!({"paths": "x"}))
            .unwrap_err()
            .code,
        "bad_args"
    );
    engine.shutdown();
}

/// `clip.copy` puts the clips on the desktop clipboard (its text is put back after), so
/// it runs with `cargo xtask e2e`.
#[test]
#[ignore = "e2e: uses the desktop clipboard"]
fn clip_copy_puts_files_on_the_clipboard() {
    let dir = tempfile::tempdir().unwrap();
    let clips = dir.path().join("clips");
    let one = clips.join("Apex").join("Scout_1.mp4");
    let two = clips.join("Scout_2.mp4");
    clip(&one, 60);
    clip(&two, 60);
    let (engine, rx) = start(settings(dir.path()));
    next(&rx, "ready");
    let before = klip_platform::clipboard_text().ok();
    let r = engine.handle("clip.copy", &json!({"paths": [one, two]}));
    let back = klip_platform::copied_files();
    if let Some(text) = before {
        klip_platform::set_clipboard_text(&text).unwrap();
    }
    assert_eq!(r.unwrap()["copied"], 2);
    let names: Vec<String> = back
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Scout_1.mp4", "Scout_2.mp4"]);

    // A recording still being written is refused.
    let mut sidecar = two.as_os_str().to_owned();
    sidecar.push(klip_library::RECORDING_SIDECAR);
    std::fs::write(&sidecar, b"").unwrap();
    let e = engine
        .handle("clip.copy", &json!({"paths": [two]}))
        .unwrap_err();
    assert_eq!(e.code, "busy");
    engine.shutdown();
}
