//! M5 acceptance (hardware): full-session recording of the test game.
//!
//! - `killed_recorder_leaves_a_playable_recording`: `record --session`, two bookmarks,
//!   then the recorder is killed hard. The file on disk plays as it is (ffmpeg decodes a
//!   continuous run of frames with audio); `klip-cli finalize` turns it into a regular
//!   MP4 with the bookmarks as chapters.
//! - `stopped_recording_is_final_and_in_sync`: `session start` / `session stop` → a
//!   regular MP4 with every captured frame and audio within one frame of the video.
//!
//! Needs a GPU encoder, an interactive desktop, audio output and ffmpeg/ffprobe on PATH.
//! Run with `cargo xtask e2e`.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::Duration;

use common::{
    KillOnDrop, capture_api_args, capture_args, check_portal_stream, cli, game, run_json,
    spawn_ready, stop_recorder,
};
use klip_testkit::{audio, continuity, oracle, sync};
use serde_json::Value;

const GAME_FPS: u32 = 120;
const CAPTURE_FPS: u32 = 60;

struct Rig {
    tmp: tempfile::TempDir,
    discovery: PathBuf,
    frames_log: PathBuf,
    tap_log: PathBuf,
    beep_log: PathBuf,
    game: KillOnDrop,
    rec: KillOnDrop,
    rec_ready: Value,
}

/// The beeping test game and a recorder writing recordings to `<tmp>/rec`.
fn start(secs: u64, extra: &[&str]) -> Rig {
    assert!(
        oracle::available("ffprobe") && oracle::available("ffmpeg"),
        "ffmpeg/ffprobe must be on PATH"
    );
    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let frames_log = tmp.path().join("frames.csv");
    let beep_log = tmp.path().join("beeps.csv");
    let tap_log = tmp.path().join("tap.csv");
    let (game_child, ready) = spawn_ready(
        game()
            .args(["--background", "--fps", &GAME_FPS.to_string()])
            .args(["--width", "1280", "--height", "720"])
            .args(["--duration", &format!("{}s", secs + 40)])
            .arg("--frames-log")
            .arg(&frames_log)
            .arg("--beep-log")
            .arg(&beep_log)
            .arg("--tap-log")
            .arg(&tap_log),
        Duration::from_secs(30),
    );
    let game = KillOnDrop(game_child);
    println!("game: {ready}");
    let (rec_child, rec_ready) = spawn_ready(
        cli(&discovery)
            .args([
                "record",
                "--buffer",
                "30s",
                "--fps",
                &CAPTURE_FPS.to_string(),
            ])
            .args(capture_args(&ready))
            .args(["--max-size", "1280x720"])
            // Linux: the portal doesn't say whose screen it is; name the game for its audio.
            .args(if cfg!(windows) {
                vec![]
            } else {
                vec!["--game", "klip-testgame"]
            })
            .args(extra)
            .args(capture_api_args())
            .arg("--out")
            .arg(tmp.path().join("clips"))
            .arg("--session-dir")
            .arg(tmp.path().join("rec")),
        Duration::from_secs(60),
    );
    let rec = KillOnDrop(rec_child);
    println!("recorder: {rec_ready}");
    check_portal_stream(&rec_ready, &ready);
    Rig {
        tmp,
        discovery,
        frames_log,
        tap_log,
        beep_log,
        game,
        rec,
        rec_ready,
    }
}

fn stop_game(game: &mut Child) {
    // The game flushes its logs once a second.
    std::thread::sleep(Duration::from_millis(1500));
    let _ = game.kill();
    let _ = game.wait();
}

/// Frame continuity of a recording against the game's frame log (as in M1). Returns the
/// report; allows `torn` unreadable frames (a frame cut by a kill).
fn check_continuity(path: &Path, frames_log: &Path, torn: usize) -> continuity::ContinuityReport {
    let counters = oracle::decode_counters(path, 640, 360).unwrap();
    let report = continuity::analyze(&counters);
    println!("continuity: {report}");
    assert!(report.unreadable <= torn, "unreadable frames: {report}");
    assert_eq!(report.backwards, 0, "frames out of order: {report}");
    let (first, last) = (report.first.unwrap(), report.last.unwrap());
    let game_times = sync::parse_log(&std::fs::read_to_string(frames_log).unwrap());
    let allowed =
        continuity::allowed_repeats(&game_times, first, last, 1.0 / f64::from(CAPTURE_FPS));
    if allowed > 0 {
        println!("WARNING: the test game stalled (GPU busy?): {allowed} repeats allowed");
    }
    assert!(report.duplicates <= allowed, "repeats: {report}");
    let step = GAME_FPS / CAPTURE_FPS;
    assert_eq!(report.steps_over(step * 2 + 1), 0, "gaps: {report}");
    report
}

fn streams_are_video_and_game_audio(path: &Path) {
    let streams = oracle::probe_streams(path).unwrap();
    assert_eq!(streams.len(), 2, "{streams:?}");
    assert_eq!(streams[0].codec_name.as_deref(), Some("h264"));
    assert_eq!(
        (streams[0].width, streams[0].height),
        (Some(1280), Some(720))
    );
    assert_eq!(streams[1].codec_name.as_deref(), Some("aac"));
    assert_eq!(
        streams[1].tags.get("handler_name").map(String::as_str),
        Some("Game")
    );
}

#[test]
#[ignore = "hardware: needs GPU encoder, desktop session, audio output, ffmpeg; run via `cargo xtask e2e`"]
fn killed_recorder_leaves_a_playable_recording() {
    let mut rig = start(12, &["--session"]);
    let path = PathBuf::from(rig.rec_ready["session"].as_str().unwrap());
    std::thread::sleep(Duration::from_secs(4));
    let b1 = run_json(cli(&rig.discovery).arg("bookmark"));
    std::thread::sleep(Duration::from_secs(3));
    let b2 = run_json(cli(&rig.discovery).arg("bookmark"));
    println!("bookmarks: {b1} {b2}");
    std::thread::sleep(Duration::from_secs(3));
    let status = run_json(cli(&rig.discovery).arg("status"));
    println!("session: {}", status["session"]);
    // Kill hard: no chance to finish the file.
    rig.rec.0.kill().unwrap();
    let _ = rig.rec.0.wait();
    stop_game(&mut rig.game.0);

    let sidecar = PathBuf::from(format!("{}.recording.json", path.display()));
    assert!(sidecar.exists(), "the sidecar marks it unfinished");
    let boxes = oracle::top_level_boxes(&path);
    println!("killed file boxes: {boxes:?}");

    // Plays as it is: video and audio, continuous frames from the start.
    streams_are_video_and_game_audio(&path);
    let killed = check_continuity(&path, &rig.frames_log, 1);
    assert!(
        killed.frames as f64 >= 8.5 * f64::from(CAPTURE_FPS),
        "about 10 s recorded: {killed}"
    );

    // Finalise (what the next `record` does on its own).
    let fin = run_json(cli(&rig.discovery).arg("finalize").arg(&path));
    println!("finalize: {fin}");
    assert_eq!(fin["already_final"], false);
    assert!(!sidecar.exists());
    let boxes = oracle::top_level_boxes(&path).unwrap();
    assert_eq!(boxes.last().map(String::as_str), Some("moov"));
    assert!(!boxes.iter().any(|b| b == "moof"), "{boxes:?}");
    streams_are_video_and_game_audio(&path);
    let fin_report = check_continuity(&path, &rig.frames_log, 0);
    assert_eq!(fin_report.first, killed.first);
    // At most the fragment being written when killed is lost (1 s at most).
    assert!(
        fin_report.frames + CAPTURE_FPS as usize + 1 >= killed.frames,
        "{fin_report} vs {killed}"
    );
    let chapters = oracle::chapters(&path).unwrap();
    println!("chapters: {chapters:?}");
    assert_eq!(chapters.len(), 2);
    for (ch, b) in chapters.iter().zip([&b1, &b2]) {
        let want = b["recording_ms"].as_f64().unwrap() / 1000.0;
        assert!(
            (ch.0 - want).abs() < 0.002,
            "chapter {ch:?} vs bookmark {want}"
        );
    }
    drop(rig.tmp);
}

#[test]
#[ignore = "hardware: needs GPU encoder, desktop session, audio output, ffmpeg; run via `cargo xtask e2e`"]
fn stopped_recording_is_final_and_in_sync() {
    let secs = 12;
    let mut rig = start(secs, &[]);
    std::thread::sleep(Duration::from_secs(2));
    let started = run_json(cli(&rig.discovery).args(["session", "start"]));
    println!("started: {started}");
    std::thread::sleep(Duration::from_secs(secs));
    let stopped = run_json(cli(&rig.discovery).args(["session", "stop"]));
    println!("stopped: {stopped}");
    stop_recorder(&rig.discovery, &mut rig.rec);
    stop_game(&mut rig.game.0);

    let files = stopped["summary"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{stopped}");
    assert_eq!(files[0]["error"], Value::Null);
    let path = PathBuf::from(files[0]["path"].as_str().unwrap());
    assert_eq!(path.to_str(), started["path"].as_str());
    assert!(!PathBuf::from(format!("{}.recording.json", path.display())).exists());
    let boxes = oracle::top_level_boxes(&path).unwrap();
    assert_eq!(boxes.first().map(String::as_str), Some("ftyp"));
    assert_eq!(boxes.last().map(String::as_str), Some("moov"));
    assert!(!boxes.iter().any(|b| b == "moof"));
    streams_are_video_and_game_audio(&path);
    let streams = oracle::probe_streams(&path).unwrap();
    let (vd, ad) = (
        streams[0].duration_secs().unwrap(),
        streams[1].duration_secs().unwrap(),
    );
    println!("durations: video {vd:.3} s, audio {ad:.3} s");
    assert!((vd - secs as f64).abs() < 1.2, "video {vd} s");
    // Stopping cuts both where the video ended (audio got time to catch up).
    assert!((ad - vd).abs() < 0.03, "audio {ad} s vs video {vd} s");
    let report = check_continuity(&path, &rig.frames_log, 0);
    assert!(report.frames as f64 >= (secs as f64 - 1.0) * f64::from(CAPTURE_FPS));

    // A/V sync, measured like M2: beep onsets in the file vs the frame showing the flash.
    let times = oracle::video_frame_times(&path).unwrap();
    let counters = oracle::decode_counters(&path, 640, 360).unwrap();
    assert_eq!(times.len(), counters.len());
    let frames: Vec<(Option<u32>, f64)> = counters.into_iter().zip(times).collect();
    let game_frames = sync::parse_log(&std::fs::read_to_string(&rig.frames_log).unwrap());
    let sorted = |p: &Path| {
        let mut v: Vec<f64> = sync::parse_log(&std::fs::read_to_string(p).unwrap())
            .into_values()
            .collect();
        v.sort_by(f64::total_cmp);
        v
    };
    let (tapped, predicted) = (sorted(&rig.tap_log), sorted(&rig.beep_log));
    let beeps = if tapped.len() * 2 >= predicted.len() {
        tapped
    } else {
        predicted
    };
    let audio_start = oracle::frame_times(&path, "a:0").unwrap()[0];
    let pcm = oracle::decode_audio_mono(&path, 0, 48_000).unwrap();
    let onsets: Vec<f64> = audio::onsets(&pcm, 48_000, audio::ONSET_THRESHOLD, 0.3)
        .into_iter()
        .map(|t| t + audio_start)
        .collect();
    let sync = sync::analyze(&frames, &game_frames, &beeps, &onsets, 0.25).unwrap();
    println!("sync: {sync}");
    println!("errors ms: {:.1?}", sync.errors_ms);
    let frame_ms = 1000.0 / f64::from(CAPTURE_FPS);
    assert!(
        sync.matched() + 1 >= sync.beeps_expected,
        "beeps missing: {sync}"
    );
    assert!(
        sync.max_abs_ms() <= frame_ms,
        "A/V error above one frame ({frame_ms:.1} ms): {sync}"
    );
    drop(rig.tmp);
}
