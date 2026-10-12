//! M8 acceptance: the editor and the export over a real clip. The daemon follows the
//! test game and saves a clip; a project (trim, a 2x piece, a text cue) is set through
//! the engine; a Discord export runs in the media worker; the file is under the limit,
//! every output frame shows the frame the plan says (the test game's barcode counters,
//! decoded with ffmpeg), and the Library lists the export next to its clip.
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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{KillOnDrop, call, cli, game, spawn_daemon, spawn_ready};
use klip_edit::{Render, SourceInfo};
use klip_testkit::oracle;
use serde_json::{Value, json};

fn clip_of<'a>(list: &'a Value, key: &str) -> &'a Value {
    list["clips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == key)
        .unwrap_or_else(|| panic!("{key} not listed: {list}"))
}

/// Frame times in whole ms (from ffprobe's seconds, via micros so 1/60 s grids round
/// the same way the engine floors its ns timestamps).
fn frame_ms(path: &Path) -> Vec<u64> {
    oracle::video_frame_times(path)
        .unwrap()
        .iter()
        .map(|t| ((t * 1e6).round() as u64) / 1000)
        .collect()
}

#[test]
#[ignore = "hardware: needs GPU encoder + decoder, desktop session, ffmpeg; run via `cargo xtask e2e`"]
fn editor_project_and_discord_export_over_a_real_clip() {
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
            "export": {"discord_limit_mb": 10},
            "games": {
                "buffer": "games",
                "detect_stores": false,
                "user_games": [{"exe": "klip-testgame.exe", "name": "Scout Test Game"}],
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

    // A real clip from the test game: 10 s at 720p60.
    let (game_child, _game_ready) = spawn_ready(
        game().args(["--background", "--fps", "120"]).args([
            "--width",
            "1280",
            "--height",
            "720",
            "--duration",
            "90s",
        ]),
        Duration::from_secs(30),
    );
    let mut game_proc = KillOnDrop(game_child);
    events.named("recorder_started", Duration::from_secs(60));
    std::thread::sleep(Duration::from_secs(12));
    let saved = call(&discovery, "clip.save", json!({"last_ms": 10_000}));
    let clip = PathBuf::from(saved["path"].as_str().unwrap());
    events.wait_for(Duration::from_secs(30), |e| e["event"] == "clip_saved");
    let _ = game_proc.0.kill();
    let _ = game_proc.0.wait();
    events.named("recorder_stopped", Duration::from_secs(15));
    let clip_s = clip.to_string_lossy().to_string();

    // The project: keep 1–9 s of the clip, the middle at 2x, a caption for 3 s.
    let got = call(&discovery, "editor.project.get", json!({"path": clip_s}));
    assert_eq!(got["status"], "new", "{got}");
    let source: SourceInfo = serde_json::from_value(got["source"].clone()).unwrap();
    assert!(source.duration_ms >= 9_000, "{source:?}");
    let mut project = got["project"].clone();
    project["pieces"] = json!([
        {"start_ms": 1_000, "end_ms": 4_000, "speed": 1},
        {"start_ms": 4_000, "end_ms": 7_000, "speed": 2},
        {"start_ms": 7_000, "end_ms": 9_000, "speed": 1},
    ]);
    project["text"] = json!([{"id": 1, "preset": "caption", "text": "clutch", "position": "bottom", "start_ms": 500, "end_ms": 3_500}]);
    let set = call(
        &discovery,
        "editor.project.set",
        json!({"path": clip_s, "project": project}),
    );
    assert_eq!(set["status"], "saved", "{set}");
    assert_eq!(set["project"]["pieces"].as_array().unwrap().len(), 3);
    let render = Render::new(
        &serde_json::from_value(set["project"].clone()).unwrap(),
        &source,
    );
    assert_eq!(render.output_ms, 3_000 + 1_500 + 2_000);

    // The preview for an H.264 clip is the clip itself.
    let media = call(&discovery, "editor.proxy", json!({"path": clip_s}));
    assert_eq!(media["status"], "original", "{media}");

    // Export: Discord, in the media worker.
    let started_at = Instant::now();
    let started = call(
        &discovery,
        "export.start",
        json!({"path": clip_s, "preset": "discord"}),
    );
    let id = started["id"].as_u64().unwrap();
    let out = PathBuf::from(started["out"].as_str().unwrap());
    assert_eq!(
        out.file_name().unwrap().to_string_lossy(),
        format!(
            "{} - Discord.mp4",
            clip.file_stem().unwrap().to_string_lossy()
        )
    );
    let busy = cli(&discovery)
        .args([
            "call",
            "export.start",
            &json!({"path": clip_s, "preset": "youtube"}).to_string(),
        ])
        .output()
        .unwrap();
    let status = call(&discovery, "export.status", Value::Null);
    if status["active"].is_object() {
        assert!(
            !busy.status.success(),
            "a second export must be refused while one runs"
        );
    }
    let mut progress = Vec::new();
    let done = loop {
        let e = events.wait_for(Duration::from_secs(180), |e| {
            (e["event"] == "export_progress" || e["event"] == "export_done") && e["id"] == id
        });
        if e["event"] == "export_done" {
            break e;
        }
        progress.push((
            e["pass"].as_u64().unwrap(),
            e["frames_done"].as_u64().unwrap(),
            e["frames_total"].as_u64().unwrap(),
        ));
    };
    let progress_seen = progress.len();
    println!(
        "export: {:?}, {progress_seen} progress events, {}",
        started_at.elapsed(),
        done["summary"]
    );
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["path"], json!(out.to_string_lossy()));
    let bytes = std::fs::metadata(&out).unwrap().len();
    assert!(bytes <= 10_000_000, "{bytes} bytes");
    assert!(!out.with_extension("mp4.part").exists());
    let last = call(&discovery, "export.status", Value::Null);
    assert_eq!(last["last"]["ok"], true, "{last}");
    assert!(last["active"].is_null());

    // Every output frame shows the frame the plan says: source counters by frame time,
    // the export's counters in order.
    let src_times = frame_ms(&clip);
    let src_counters = oracle::decode_counters(&clip, 640, 360).unwrap();
    assert_eq!(src_times.len(), src_counters.len());
    let out_counters = oracle::decode_counters(&out, 640, 360).unwrap();
    let fps = source.fps.unwrap_or(60.0).round() as u32;
    assert_eq!(
        out_counters.len() as u64,
        render.frame_count(fps),
        "output frame count"
    );
    let mut mismatches = Vec::new();
    let mut unreadable = 0;
    for (i, got) in out_counters.iter().enumerate() {
        let src_ms = render.frame_source_ms(i as u64, fps).unwrap();
        let k = src_times.iter().rposition(|t| *t <= src_ms).unwrap_or(0);
        match (got, src_counters[k]) {
            (Some(g), Some(want)) if *g != want => mismatches.push((i, *g, want)),
            (None, _) | (_, None) => unreadable += 1,
            _ => {}
        }
    }
    println!(
        "{} output frames, {} mismatches, {unreadable} unreadable; first mismatches {:?}",
        out_counters.len(),
        mismatches.len(),
        &mismatches[..mismatches.len().min(8)]
    );
    assert!(
        unreadable <= out_counters.len() / 50,
        "{unreadable} unreadable frames"
    );
    assert!(
        mismatches.len() <= out_counters.len() / 50,
        "{} mismatches",
        mismatches.len()
    );
    let report = klip_testkit::continuity::analyze(&out_counters);
    assert_eq!(report.backwards, 0, "{report}");

    // The worker's progress lines (IMPROVEMENTS G3): never backwards within a pass, and
    // every pass counts the frames the plan has.
    assert!(progress_seen > 0, "no progress from the media worker");
    for w in progress.windows(2) {
        let ((p0, d0, _), (p1, d1, _)) = (w[0], w[1]);
        assert!(
            p1 > p0 || (p1 == p0 && d1 >= d0),
            "progress went back: {w:?}"
        );
    }
    for (pass, done_frames, total) in &progress {
        assert_eq!(*total, render.frame_count(fps), "pass {pass}");
        assert!(done_frames <= total, "pass {pass}: {done_frames}/{total}");
    }

    // The Library lists the export beside its clip, with its info.
    events.wait_for(Duration::from_secs(10), |e| {
        e["event"] == "library_changed" && e["reason"] == "exported"
    });
    let list = call(&discovery, "library.list", Value::Null);
    assert_eq!(list["total"], 2, "{list}");
    let key = format!(
        "Scout Test Game/{}",
        out.file_name().unwrap().to_string_lossy()
    );
    let c = clip_of(&list, &key);
    assert_eq!(c["game"], "Scout Test Game");
    let ms = c["info"]["duration_ms"].as_u64().unwrap();
    assert!((6_400..=6_600).contains(&ms), "{ms} ms");
    assert_eq!(c["info"]["codec"], "H.264");
    assert_eq!(c["info"]["audio_tracks"], 1);
    events.wait_for(Duration::from_secs(60), |e| {
        e["event"] == "thumbs_ready" && e["key"] == key.as_str()
    });

    // A second job on the same media worker, cancelled at its first progress line: an
    // error, and neither the file nor its part file is left (IMPROVEMENTS G3).
    let started = call(
        &discovery,
        "export.start",
        json!({"path": clip_s, "preset": "youtube"}),
    );
    let id2 = started["id"].as_u64().unwrap();
    let out2 = PathBuf::from(started["out"].as_str().unwrap());
    let first = events.wait_for(Duration::from_secs(120), |e| {
        (e["event"] == "export_progress" || e["event"] == "export_done") && e["id"] == id2
    });
    assert_eq!(first["event"], "export_progress", "{first}");
    call(&discovery, "export.cancel", json!({"id": id2}));
    let done2 = events.wait_for(Duration::from_secs(60), |e| {
        e["event"] == "export_done" && e["id"] == id2
    });
    assert_eq!(done2["ok"], false, "{done2}");
    assert!(!out2.exists(), "a cancelled export left {}", out2.display());
    assert!(!out2.with_extension("mp4.part").exists());

    // The project follows a rename; delete removes both.
    let r = call(
        &discovery,
        "clip.rename",
        json!({"path": clip_s, "name": "Edited"}),
    );
    let again = call(&discovery, "editor.project.get", json!({"path": r["path"]}));
    assert_eq!(again["status"], "saved", "{again}");
    assert_eq!(again["project"]["pieces"].as_array().unwrap().len(), 3);
    let d = call(
        &discovery,
        "clip.delete",
        json!({"paths": [r["path"], out], "permanent": true}),
    );
    assert_eq!(d["removed"], 2, "{d}");
    let list = call(&discovery, "library.list", Value::Null);
    assert_eq!(list["total"], 0);
    assert!(
        !clips
            .join(".klip")
            .join("projects")
            .join("Scout Test Game%2FEdited.mp4.json")
            .exists()
    );
}
