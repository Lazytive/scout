//! M7 acceptance: the library over real clips. The daemon follows the test game and saves
//! two clips; `library.list` reads them (duration, size, fps, codec); the visible-card
//! request makes a hover-scrub strip in the media worker (a child process) even while the
//! game runs, the background work waits for the game to end; strips are 16 distinct
//! frames; rename keeps the star, tags and thumbnails; delete removes the clip.
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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{KillOnDrop, call, cli, game, spawn_daemon, spawn_ready};
use klip_testkit::oracle;
use serde_json::{Value, json};

/// Width and height from a JPEG's start-of-frame marker.
fn jpeg_size(d: &[u8]) -> (u32, u32) {
    let mut i = 2;
    while i + 9 < d.len() {
        assert_eq!(d[i], 0xFF, "JPEG marker");
        let marker = d[i + 1];
        let len = usize::from(u16::from_be_bytes([d[i + 2], d[i + 3]]));
        if (0xC0..=0xC3).contains(&marker) {
            let h = u16::from_be_bytes([d[i + 5], d[i + 6]]);
            let w = u16::from_be_bytes([d[i + 7], d[i + 8]]);
            return (u32::from(w), u32::from(h));
        }
        i += 2 + len;
    }
    panic!("no SOF in the JPEG");
}

fn clip_of<'a>(list: &'a Value, key: &str) -> &'a Value {
    list["clips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == key)
        .unwrap_or_else(|| panic!("{key} not listed: {list}"))
}

#[test]
#[ignore = "hardware: needs GPU encoder + decoder, desktop session, ffmpeg; run via `cargo xtask e2e`"]
fn library_over_real_clips() {
    assert!(oracle::available("ffmpeg"), "ffmpeg must be on PATH");
    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let settings = tmp.path().join("settings.json");
    let clips = tmp.path().join("clips");
    std::fs::write(
        &settings,
        serde_json::to_vec_pretty(&json!({
            "app": {"control_channel": true},
            "clips": {"folder": clips, "per_game_folders": true},
            "recording": {
                "buffer_secs": 30,
                "quality": {"preset": "custom", "custom": {
                    "max_width": 1280, "max_height": 720, "fps": 60, "bitrate_kbps": 12000,
                }},
            },
            "audio": {"desktop": "game", "voice": false, "mic": false},
            "hotkeys": [],
            "overlay": {"mode": "off", "sound": false, "notification": false},
            "games": {
                "buffer": "games",
                // Only the test game: never a game the human is playing.
                "detect_stores": false,
                "user_games": [{"exe": "klip-testgame.exe", "name": "Klip Test Game"}],
            },
        }))
        .unwrap(),
    )
    .unwrap();

    let (_daemon, mut events) = spawn_daemon(
        cli(&discovery)
            .arg("daemon")
            .arg("--settings")
            .arg(&settings),
    );
    events.named("ready", Duration::from_secs(60));
    let empty = call(&discovery, "library.list", Value::Null);
    assert_eq!(empty["total"], 0, "{empty}");

    let (game_child, _game_ready) = spawn_ready(
        game().args(["--background", "--fps", "120"]).args([
            "--width",
            "1280",
            "--height",
            "720",
            "--duration",
            "120s",
        ]),
        Duration::from_secs(30),
    );
    let mut game_proc = KillOnDrop(game_child);
    events.named("recorder_started", Duration::from_secs(60));
    std::thread::sleep(Duration::from_secs(10));
    let mut saved: Vec<PathBuf> = Vec::new();
    for _ in 0..2 {
        let r = call(&discovery, "clip.save", json!({"last_ms": 8000}));
        saved.push(PathBuf::from(r["path"].as_str().unwrap()));
        std::thread::sleep(Duration::from_secs(3));
    }
    let key = |p: &Path| {
        p.strip_prefix(&clips)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    let (k1, k2) = (key(&saved[0]), key(&saved[1]));

    // While the game runs: listed with their info; posters come from the save itself,
    // strips wait (background work pauses for a game).
    for k in [&k1, &k2] {
        events.wait_for(Duration::from_secs(20), |e| {
            e["event"] == "thumbs_ready" && e["key"] == k.as_str() && e["poster"].is_u64()
        });
    }
    let list = call(&discovery, "library.list", Value::Null);
    assert_eq!(list["total"], 2, "{list}");
    for k in [&k1, &k2] {
        let c = clip_of(&list, k);
        assert_eq!(c["game"], "Klip Test Game");
        let info = &c["info"];
        let ms = info["duration_ms"].as_u64().unwrap();
        assert!((7_000..=9_000).contains(&ms), "{ms} ms");
        assert_eq!(
            (info["width"].clone(), info["height"].clone()),
            (json!(1280), json!(720))
        );
        assert_eq!(info["codec"], "H.264");
        let fps = info["fps"].as_f64().unwrap();
        assert!((fps - 60.0).abs() < 3.0, "{fps} fps");
        assert!(c["thumbs"]["poster"].is_u64(), "{c}");
        assert!(
            c["thumbs"]["strip"].is_null(),
            "no strip while the game runs: {c}"
        );
    }

    // A visible card: made now, in the worker process, game or not.
    let asked = Instant::now();
    call(&discovery, "library.thumbs", json!({"keys": [k1]}));
    let ready = events.wait_for(Duration::from_secs(60), |e| {
        e["event"] == "thumbs_ready" && e["key"] == k1.as_str() && e["strip"].is_u64()
    });
    assert_eq!(ready["failed"], false, "{ready}");
    println!(
        "strip for a visible card: {:?} (worker start included)",
        asked.elapsed()
    );

    // The game ends: the background work makes the other strip.
    let _ = game_proc.0.kill();
    let _ = game_proc.0.wait();
    events.named("recorder_stopped", Duration::from_secs(15));
    let ended = Instant::now();
    events.wait_for(Duration::from_secs(60), |e| {
        e["event"] == "thumbs_ready" && e["key"] == k2.as_str() && e["strip"].is_u64()
    });
    println!(
        "background strip {:?} after the game ended",
        ended.elapsed()
    );

    // The strip: 16 frames of 240 px, all different (the test game moves every frame).
    let t = call(
        &discovery,
        "library.thumb",
        json!({"key": k1, "kind": "strip"}),
    );
    let strip_path = PathBuf::from(t["path"].as_str().unwrap());
    let jpeg = std::fs::read(&strip_path).unwrap();
    let (w, h) = jpeg_size(&jpeg);
    assert_eq!((w, h), (240, 135 * 16), "strip size");
    println!("strip: {} KB", jpeg.len() / 1024);
    // (No `-ss` here: ffmpeg 9 gives nothing for a seek in a still image.)
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&strip_path)
        .args(["-frames:v", "1", "-pix_fmt", "gray", "-f", "rawvideo", "-"])
        .output()
        .unwrap();
    let gray = out.stdout;
    assert_eq!(
        gray.len(),
        240 * 135 * 16,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tile = 240 * 135;
    for i in 1..16 {
        let a = &gray[(i - 1) * tile..i * tile];
        let b = &gray[i * tile..(i + 1) * tile];
        let diff: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| f64::from(x.abs_diff(*y)))
            .sum::<f64>()
            / tile as f64;
        assert!(
            diff > 1.0,
            "frames {} and {i} look the same ({diff:.2})",
            i - 1
        );
    }

    // Star, tags, rename: everything follows the clip.
    let p1 = saved[0].to_string_lossy().to_string();
    call(
        &discovery,
        "clip.star",
        json!({"path": p1, "starred": true}),
    );
    call(
        &discovery,
        "clip.tags",
        json!({"path": p1, "tags": ["e2e"]}),
    );
    let r = call(
        &discovery,
        "clip.rename",
        json!({"path": p1, "name": "Test clip"}),
    );
    let new_key = r["key"].as_str().unwrap().to_string();
    assert_eq!(new_key, "Klip Test Game/Test clip.mp4");
    let list = call(&discovery, "library.list", json!({"search": "test clip"}));
    assert_eq!(list["matched"], 1, "{list}");
    let c = clip_of(&list, &new_key);
    assert_eq!(
        (c["starred"].clone(), c["tags"].clone()),
        (json!(true), json!(["e2e"]))
    );
    assert!(
        c["thumbs"]["poster"].is_u64() && c["thumbs"]["strip"].is_u64(),
        "{c}"
    );

    // Delete (permanently: a test never fills the real Recycle Bin).
    let d = call(
        &discovery,
        "clip.delete",
        json!({"paths": [saved[1]], "permanent": true}),
    );
    assert_eq!(d["removed"], 1, "{d}");
    assert!(!saved[1].exists());
    let list = call(&discovery, "library.list", Value::Null);
    assert_eq!(list["total"], 1);
    let gone = call(&discovery, "library.thumbs", json!({"keys": [k2]}));
    assert_eq!(gone["queued"], 0);

    let _ = cli(&discovery).arg("stop").status();
}
