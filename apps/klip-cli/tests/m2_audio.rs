//! M2 acceptance (hardware): audio tracks and A/V sync.
//!
//! - `game_audio_is_in_sync`: the test game (120 fps, beeping at every flash) + `klip-cli
//!   record` with game audio (per-process loopback of the game) → a clip whose single AAC
//!   track is in sync with the video to within one capture frame. The video timeline comes
//!   from the barcodes and the game's frame log (written once the GPU finished each frame),
//!   the expected beep times from the game's loopback-tap log (when each beep passed the
//!   audio engine, by WASAPI's capture timestamps; see `klip_testkit::sync`). The test
//!   also prints how late the recorder stamps video and audio on the shared
//!   QueryPerformanceCounter timeline.
//! - `mic_adds_mix_and_separate_tracks`: with `--mic`, the clip has Mix (default), Game and
//!   Microphone tracks.
//!
//! Needs a GPU encoder, a desktop session, an audio output device and ffmpeg/ffprobe on
//! PATH; the beeps are audible. Run with `cargo xtask e2e`. `KLIP_E2E_CLIP_SECS` sets the
//! clip length (default 30).

// Test code: panicking helpers are allowed (AGENTS.md rule 3 applies outside tests).
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{KillOnDrop, capture_api_args, cli, game, run_json, spawn_ready, stop_recorder};
use klip_testkit::{audio, oracle, sync};
use serde_json::Value;

const GAME_FPS: u32 = 120;
const CAPTURE_FPS: u32 = 60;

struct Run {
    clip: PathBuf,
    saved: Value,
    status: Value,
    frames_log: PathBuf,
    beep_log: PathBuf,
    tap_log: PathBuf,
    game: Value,
    _tmp: tempfile::TempDir,
}

/// Run the beeping test game, record it with `extra` recorder args, save the last
/// `clip_secs` and stop everything.
fn record_game(clip_secs: u64, extra: &[&str]) -> Run {
    assert!(
        oracle::available("ffprobe") && oracle::available("ffmpeg"),
        "ffmpeg/ffprobe must be on PATH"
    );
    let tmp = tempfile::tempdir().unwrap();
    let discovery = tmp.path().join("control.json");
    let clips = tmp.path().join("clips");
    let frames_log = tmp.path().join("frames.csv");
    let beep_log = tmp.path().join("beeps.csv");
    let tap_log = tmp.path().join("tap.csv");
    let (game, ready) = spawn_ready(
        game()
            .args(["--background", "--fps", &GAME_FPS.to_string()])
            .args(["--width", "1280", "--height", "720"])
            .args(["--duration", &format!("{}s", clip_secs + 30)])
            .arg("--frames-log")
            .arg(&frames_log)
            .arg("--beep-log")
            .arg(&beep_log)
            .arg("--tap-log")
            .arg(&tap_log),
        Duration::from_secs(30),
    );
    let mut game = KillOnDrop(game);
    println!("game: {ready}");
    let hwnd = ready["hwnd"].as_u64().unwrap();
    assert!(
        ready["beep_rate"].as_u64().is_some(),
        "game has no audio output"
    );

    let (rec, rec_ready) = spawn_ready(
        cli(&discovery)
            .args([
                "record",
                "--buffer",
                "60s",
                "--fps",
                &CAPTURE_FPS.to_string(),
            ])
            .args(["--window-handle", &hwnd.to_string()])
            .args(extra)
            .args(capture_api_args())
            .arg("--out")
            .arg(&clips),
        Duration::from_secs(60),
    );
    let mut rec = KillOnDrop(rec);
    println!("recorder: {rec_ready}");
    std::thread::sleep(Duration::from_secs(clip_secs + 3));
    let saved = run_json(cli(&discovery).args(["clip", "--last", &format!("{clip_secs}s")]));
    let status = run_json(cli(&discovery).arg("status"));
    stop_recorder(&discovery, &mut rec);
    // The game flushes its logs once a second; give it a moment, then stop it.
    std::thread::sleep(Duration::from_millis(1500));
    let _ = game.0.kill();
    let _ = game.0.wait();
    println!("clip: {saved}");
    println!("audio status: {}", status["audio"]);
    println!("capture: {}", status["capture"]);
    Run {
        clip: PathBuf::from(saved["path"].as_str().unwrap()),
        saved,
        status,
        frames_log,
        beep_log,
        tap_log,
        game: ready,
        _tmp: tmp,
    }
}

fn clip_secs() -> u64 {
    std::env::var("KLIP_E2E_CLIP_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
}

fn read_log(path: &Path) -> std::collections::HashMap<u32, f64> {
    sync::parse_log(&std::fs::read_to_string(path).unwrap())
}

#[test]
#[ignore = "hardware: needs GPU encoder, desktop session, audio output, ffmpeg; run via `cargo xtask e2e`"]
fn game_audio_is_in_sync() {
    let clip_secs = clip_secs();
    let run = record_game(clip_secs, &[]);
    assert_eq!(run.status["audio"]["error"], Value::Null);
    assert_eq!(run.saved["audio_tracks"], 1);
    let save_ms = run.saved["save_ms"].as_f64().unwrap();
    assert!(save_ms < 1000.0, "save took {save_ms} ms");

    // Container: H.264 video + one AAC track named Game, played by default.
    let streams = oracle::probe_streams(&run.clip).unwrap();
    assert_eq!(streams.len(), 2, "{streams:?}");
    let (v, a) = (&streams[0], &streams[1]);
    assert_eq!(v.codec_name.as_deref(), Some("h264"));
    assert_eq!(a.codec_name.as_deref(), Some("aac"));
    assert_eq!(a.sample_rate.as_deref(), Some("48000"));
    assert_eq!(a.channels, Some(2));
    assert_eq!(a.tags.get("handler_name").map(String::as_str), Some("Game"));
    assert_eq!(a.disposition.get("default"), Some(&1));
    let (vd, ad) = (v.duration_secs().unwrap(), a.duration_secs().unwrap());
    let a_start: f64 = a.start_time.as_deref().unwrap().parse().unwrap();
    println!(
        "durations: video {vd:.3} s, audio {ad:.3} s from {a_start:.3} s; waited {} ms for audio",
        run.saved["audio_wait_ms"]
    );
    // Audio starts within one AAC frame of the video and runs to its end.
    assert!(a_start < 0.022, "audio starts at {a_start}");
    assert!(a_start + ad >= vd - 0.022, "audio ends early");

    let report = sync_report(&run, 0);
    let frame_ms = 1000.0 / f64::from(CAPTURE_FPS);
    assert!(
        report.beeps_expected as u64 >= clip_secs.saturating_sub(3),
        "{report}"
    );
    assert!(
        report.matched() + 1 >= report.beeps_expected,
        "beeps missing: {report}"
    );
    assert_eq!(report.stray_onsets, 0, "unexpected sounds: {report}");
    assert!(
        report.max_abs_ms() <= frame_ms,
        "A/V error above one frame ({frame_ms:.1} ms): {report}"
    );
}

/// Diagnostic: the same measurement through system loopback (the whole output mix)
/// instead of process loopback, to compare the two capture paths. Other sounds on the
/// machine may add stray onsets, so only the latency is reported.
#[test]
#[ignore = "diagnostic: needs GPU encoder, desktop session, audio output, ffmpeg"]
fn diag_system_loopback_latency() {
    let run = record_game(10, &["--audio", "system", "--no-default-excludes"]);
    let report = sync_report(&run, 0);
    assert!(report.matched() >= 5, "{report}");
}

/// A/V sync of audio track `track` of a recorded clip against the game's logs; prints
/// the details, including per-stream stamp latency.
fn sync_report(run: &Run, track: usize) -> sync::SyncReport {
    let times = oracle::video_frame_times(&run.clip).unwrap();
    let counters = oracle::decode_counters(&run.clip, 640, 360).unwrap();
    assert_eq!(times.len(), counters.len());
    let frames: Vec<(Option<u32>, f64)> = counters.into_iter().zip(times).collect();
    let game_frames = read_log(&run.frames_log);
    let sorted = |m: std::collections::HashMap<u32, f64>| {
        let mut v: Vec<f64> = m.into_values().collect();
        v.sort_by(f64::total_cmp);
        v
    };
    let predicted = sorted(read_log(&run.beep_log));
    let tapped = sorted(read_log(&run.tap_log));
    // The tap measurement is the reference; cpal's prediction is the fallback.
    let beeps = if tapped.len() * 2 >= predicted.len() {
        &tapped
    } else {
        println!(
            "tap log too short ({}), using predicted beep times",
            tapped.len()
        );
        &predicted
    };
    let audio_start = oracle::frame_times(&run.clip, &format!("a:{track}")).unwrap()[0];
    let pcm = oracle::decode_audio_mono(&run.clip, track, 48_000).unwrap();
    let onsets: Vec<f64> = audio::onsets(&pcm, 48_000, audio::ONSET_THRESHOLD, 0.3)
        .into_iter()
        .map(|t| t + audio_start)
        .collect();
    println!(
        "{} beeps logged, {} onsets found, audio starts at {audio_start:.4} s",
        beeps.len(),
        onsets.len()
    );
    let report = sync::analyze(&frames, &game_frames, beeps, &onsets, 0.25).unwrap();
    println!("sync: {report}");
    println!("errors ms: {:.1?}", report.errors_ms);
    if let Some(p) = sync::analyze(&frames, &game_frames, &predicted, &onsets, 0.25) {
        println!("vs predicted beep times: {p}");
    }
    print_stamp_latency(run, &frames, &game_frames, beeps, &onsets);
    report
}

/// How late the recorder stamps each stream, on the shared QueryPerformanceCounter
/// timeline: video against the game's frame log, audio against the loopback tap.
fn print_stamp_latency(
    run: &Run,
    frames: &[(Option<u32>, f64)],
    game_frames: &std::collections::HashMap<u32, f64>,
    beeps: &[f64],
    onsets: &[f64],
) {
    let (Some(base), Some(start), Some(qpc), Some(game)) = (
        run.status["clock_base_ns"].as_i64(),
        run.saved["start_ns"].as_i64(),
        run.game["anchor"]["qpc_ns"].as_i64(),
        run.game["anchor"]["game_ns"].as_i64(),
    ) else {
        println!("no clock anchors; skipping the per-stream latency");
        return;
    };
    // clip time t (s) → QPC ns, minus game time g (s) → QPC ns.
    let offset_ns = (base + start - qpc + game) as f64;
    let late_ms = |t: f64, g: f64| (offset_ns + (t - g) * 1e9) / 1e6;
    let mut v: Vec<f64> = frames
        .iter()
        .filter_map(|(c, t)| Some(late_ms(*t, *game_frames.get(&(*c)?)?)))
        .collect();
    let mut a: Vec<f64> = onsets
        .iter()
        .filter_map(|o| {
            let b = beeps
                .iter()
                .min_by(|x, y| late_ms(*o, **x).abs().total_cmp(&late_ms(*o, **y).abs()))?;
            let l = late_ms(*o, *b);
            (l.abs() < 250.0).then_some(l)
        })
        .collect();
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
    };
    let (vm, am) = (median(&mut v), median(&mut a));
    println!(
        "stamp latency: video {vm:+.2} ms after the GPU finished the frame ({} frames), audio {am:+.2} ms after the loopback tap ({} beeps); A/V {:+.2} ms",
        v.len(),
        a.len(),
        am - vm
    );
}

#[test]
#[ignore = "hardware: needs GPU encoder, desktop session, audio devices, ffmpeg; run via `cargo xtask e2e`"]
fn mic_adds_mix_and_separate_tracks() {
    let run = record_game(5, &["--mic"]);
    assert_eq!(run.status["audio"]["error"], Value::Null);
    assert_eq!(run.saved["audio_tracks"], 3);
    let streams = oracle::probe_streams(&run.clip).unwrap();
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .map(|s| {
            (
                s.tags.get("handler_name").cloned().unwrap_or_default(),
                s.disposition.get("default").copied().unwrap_or(0),
            )
        })
        .collect();
    println!("audio tracks: {audio:?}");
    assert_eq!(
        audio,
        [
            ("Mix".to_string(), 1),
            ("Game".to_string(), 0),
            ("Microphone".to_string(), 0)
        ]
    );
    // The game's beeps are in the mix (the mic may add anything; only check the game).
    let pcm = oracle::decode_audio_mono(&run.clip, 0, 48_000).unwrap();
    let onsets = audio::onsets(&pcm, 48_000, audio::ONSET_THRESHOLD, 0.3);
    assert!(onsets.len() >= 3, "beeps in the mix: {onsets:?}");
}

#[test]
#[ignore = "hardware: needs GPU encoder, desktop session, audio output, ffmpeg; run via `cargo xtask e2e`"]
fn excluded_app_is_left_out_of_system_audio() {
    // The test game is the excluded app here; its beeps must not be in the clip. Other
    // sounds on the machine may be, so only beep-shaped onsets at beep times count.
    let run = record_game(
        10,
        &["--audio", "system", "--exclude-app", "klip-testgame.exe"],
    );
    assert_eq!(run.status["audio"]["error"], Value::Null);
    let source = run.status["audio"]["inputs"][0]["source"].as_str().unwrap();
    assert!(source.starts_with("system audio except pid"), "{source}");
    let times = oracle::video_frame_times(&run.clip).unwrap();
    let counters = oracle::decode_counters(&run.clip, 640, 360).unwrap();
    let frames: Vec<(Option<u32>, f64)> = counters.into_iter().zip(times).collect();
    let beeps: Vec<f64> = read_log(&run.beep_log).into_values().collect();
    let audio_start = oracle::frame_times(&run.clip, "a:0").unwrap()[0];
    let pcm = oracle::decode_audio_mono(&run.clip, 0, 48_000).unwrap();
    let onsets: Vec<f64> = audio::onsets(&pcm, 48_000, audio::ONSET_THRESHOLD, 0.3)
        .into_iter()
        .map(|t| t + audio_start)
        .collect();
    let report = sync::analyze(&frames, &read_log(&run.frames_log), &beeps, &onsets, 0.03).unwrap();
    println!("excluded: {report}");
    assert!(report.beeps_expected >= 7, "{report}");
    assert!(
        report.matched() <= 1,
        "the excluded game was recorded: {report}"
    );
}
