//! M9 acceptance: a montage of two recorded clips. The daemon follows the test game and
//! saves two clips seconds apart; the first is trimmed in Normal mode, the second is
//! added with `editor.montage`, split in two with a crossfade between the halves, and
//! the second half zooms in 2x. A Custom export runs in the media worker; every output
//! frame is checked against the plan (`compose_with`, the same maths the renderer
//! uses): plain frames by the test game's barcode counters, the crossfade and the zoom
//! by PSNR against references built from the source frames the plan names. The
//! Library lists the export, and the montage follows a rename of its second clip.
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
use std::process::Command;
use std::time::{Duration, Instant};

use common::{KillOnDrop, call, game, spawn_daemon, spawn_ready};
use klip_edit::{Composition, Layer, Project, Source, SourceInfo, compose_with};
use klip_testkit::{barcode, oracle};
use serde_json::{Value, json};

const FPS: u32 = 60;
const W: usize = 1280;
const H: usize = 720;

/// A source clip as the oracle sees it: frame times (s) and barcode counters.
struct Clip {
    times: Vec<f64>,
    counters: Vec<Option<u32>>,
}

impl Clip {
    fn read(path: &Path) -> Clip {
        let times = oracle::video_frame_times(path).unwrap();
        let counters = oracle::decode_counters(path, 640, 360).unwrap();
        assert_eq!(times.len(), counters.len(), "{}", path.display());
        Clip { times, counters }
    }

    /// The frame shown at source time `ms` (the last one starting at or before it;
    /// whole ms via micros, as the engine floors its ns timestamps).
    fn frame_at(&self, ms: u64) -> usize {
        self.times
            .iter()
            .rposition(|t| ((t * 1e6).round() as u64) / 1000 <= ms)
            .unwrap_or(0)
    }

    /// Frame `k` as gray, checked against its own barcode.
    fn gray(&self, path: &Path, k: usize) -> Vec<u8> {
        let g = gray_frame(path, k);
        assert_eq!(barcode::read_gray(&g, W, H), self.counters[k], "frame {k}");
        g
    }
}

/// Frame `n` of `path` (in presentation order) as `W`x`H` 8-bit gray, picked by index
/// rather than by a seek.
fn gray_frame(path: &Path, n: usize) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-vf"])
        .arg(format!("select=eq(n\\,{n}),scale={W}:{H}:flags=area"))
        .args(["-fps_mode", "passthrough", "-frames:v", "1"])
        .args(["-pix_fmt", "gray", "-f", "rawvideo", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success() && out.stdout.len() == W * H,
        "ffmpeg frame {n} of {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum::<f64>()
        / a.len().max(1) as f64;
    if mse <= 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

/// `src` zoomed `zoom`x about the centre (bilinear).
fn zoomed(src: &[u8], zoom: f64) -> Vec<u8> {
    let mut out = vec![0u8; W * H];
    let p = |x: usize, y: usize| f64::from(src[y.min(H - 1) * W + x.min(W - 1)]);
    for y in 0..H {
        for x in 0..W {
            let sx = (W as f64 / 2.0 + (x as f64 + 0.5 - W as f64 / 2.0) / zoom - 0.5).max(0.0);
            let sy = (H as f64 / 2.0 + (y as f64 + 0.5 - H as f64 / 2.0) / zoom - 0.5).max(0.0);
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
            let v = p(x0, y0) * (1.0 - fx) * (1.0 - fy)
                + p(x0 + 1, y0) * fx * (1.0 - fy)
                + p(x0, y0 + 1) * (1.0 - fx) * fy
                + p(x0 + 1, y0 + 1) * fx * fy;
            out[y * W + x] = v.round() as u8;
        }
    }
    out
}

/// What the plan says output frame `i` is.
enum Planned<'a> {
    /// One source frame as it is: (layer, source time).
    Plain(&'a Layer, u64),
    /// The top layer is fading in over another.
    Blend,
    /// The top layer is zoomed.
    Zoom,
}

fn planned(c: &Composition, i: u64) -> Planned<'_> {
    let t = c.frame_out_ms(i, FPS).unwrap();
    let top = *c.visible_at(t).last().expect("something is on screen");
    if top.opacity_at(t) < 1.0 {
        Planned::Blend
    } else if (top.transform_at(t).zoom - 1.0).abs() > 1e-3 {
        Planned::Zoom
    } else {
        Planned::Plain(top, top.source_ms(t).unwrap())
    }
}

#[test]
#[ignore = "hardware: needs GPU encoder + decoder, desktop session, ffmpeg; run via `cargo xtask e2e`"]
fn montage_of_two_recorded_clips_with_a_crossfade_and_a_zoom() {
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
                    "max_width": W, "max_height": H, "fps": FPS, "bitrate_kbps": 12000,
                }},
            },
            "audio": {"desktop": "game", "voice": false, "mic": false},
            "hotkeys": [],
            "overlay": {"mode": "off", "sound": false, "notification": false},
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
        common::cli(&discovery)
            .arg("daemon")
            .arg("--settings")
            .arg(&settings),
    );
    events.named("ready", Duration::from_secs(60));

    // Two clips of 6 s from one game session, 8 s apart: their counters don't overlap.
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
    let started = Instant::now();
    let mut save = |at_s: u64| {
        std::thread::sleep(Duration::from_secs(at_s).saturating_sub(started.elapsed()));
        let saved = call(&discovery, "clip.save", json!({"last_ms": 6_000}));
        let path = PathBuf::from(saved["path"].as_str().unwrap());
        events.named("clip_saved", Duration::from_secs(30));
        path
    };
    let a = save(8);
    let b = save(16);
    let _ = game_proc.0.kill();
    let _ = game_proc.0.wait();
    events.named("recorder_stopped", Duration::from_secs(15));
    let (a_s, b_s) = (
        a.to_string_lossy().to_string(),
        b.to_string_lossy().to_string(),
    );

    // Normal mode on the first clip: keep 1–5 s.
    let got = call(&discovery, "editor.project.get", json!({"path": a_s}));
    let mut project = got["project"].clone();
    project["pieces"] = json!([{"start_ms": 1_000, "end_ms": 5_000, "speed": 1}]);
    let set = call(
        &discovery,
        "editor.project.set",
        json!({"path": a_s, "project": project}),
    );
    assert_eq!(set["status"], "saved", "{set}");

    // The second clip appended, whole, on the montage track at 4 s.
    let m = call(
        &discovery,
        "editor.montage",
        json!({"path": a_s, "add": [b_s]}),
    );
    let b_key = format!(
        "Scout Test Game/{}",
        b.file_name().unwrap().to_string_lossy()
    );
    let b_entry = m["clips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == b_key.as_str())
        .unwrap_or_else(|| panic!("{b_key} not in the montage: {m}"));
    assert_eq!(b_entry["status"], "ok", "{b_entry}");
    let b_info: SourceInfo = serde_json::from_value(b_entry["source"].clone()).unwrap();
    assert!(b_info.duration_ms >= 5_500, "{b_info:?}");
    let mut p: Project = serde_json::from_value(m["project"].clone()).unwrap();
    let ti = p
        .advanced
        .tracks
        .iter()
        .position(|t| !t.clips.is_empty())
        .unwrap();
    let track_id = p.advanced.tracks[ti].id;
    let first = p.advanced.tracks[ti].clips[0].clone();
    assert_eq!(
        (&first.source, first.in_ms, first.out_ms, first.at_ms),
        (
            &Source::Library { key: b_key.clone() },
            0,
            b_info.duration_ms,
            4_000
        )
    );

    // Split it: b's 0–2.5 s, then b's 3–5.5 s crossfading in over 500 ms (it starts at
    // 6 s), holding 2x zoom from 1.5 s into it (7.5 s out).
    let mut second = first.clone();
    second.id = first.id + 1;
    second.in_ms = 3_000;
    second.out_ms = 5_500;
    second.at_ms = 6_500;
    second.transform = serde_json::from_value(json!([
        {"at_ms": 0, "value": {"zoom": 1.0, "x": 0.0, "y": 0.0}, "ease": "hold"},
        {"at_ms": 1_500, "value": {"zoom": 2.0, "x": 0.0, "y": 0.0}, "ease": "hold"},
    ]))
    .unwrap();
    p.advanced.tracks[ti].clips[0].out_ms = 2_500;
    p.advanced.tracks[ti].clips.push(second);
    p.advanced.transitions = serde_json::from_value(json!([
        {"track": track_id, "after_clip": first.id, "kind": "crossfade", "ms": 500},
    ]))
    .unwrap();
    let set = call(
        &discovery,
        "editor.project.set",
        json!({"path": a_s, "project": p}),
    );
    assert_eq!(set["status"], "saved", "{set}");
    assert_eq!(set["issues"], json!([]), "{set}");
    let saved: Project = serde_json::from_value(set["project"].clone()).unwrap();
    assert_eq!(
        saved.advanced, p.advanced,
        "the engine kept the edit as sent"
    );
    let a_info: SourceInfo = serde_json::from_value(set["source"].clone()).unwrap();
    let plan = compose_with(&saved, &a_info, &[(b_key.clone(), b_info.clone())]);
    assert_eq!(plan.duration_ms, 8_500);

    // Export: Custom 720p60 H.264, in the media worker.
    let t0 = Instant::now();
    let started = call(
        &discovery,
        "export.start",
        json!({"path": a_s, "preset": "custom", "custom": {
            "width": W, "height": H, "fps": FPS, "bitrate_kbps": 12000, "codec": "h264",
        }}),
    );
    let id = started["id"].as_u64().unwrap();
    let out = PathBuf::from(started["out"].as_str().unwrap());
    assert_eq!(
        out.file_name().unwrap().to_string_lossy(),
        format!("{} - Custom.mp4", a.file_stem().unwrap().to_string_lossy())
    );
    let done = events.wait_for(Duration::from_secs(180), |e| {
        e["event"] == "export_done" && e["id"] == id
    });
    println!(
        "montage export (8.5 s of 720p60 from two clips): {:.1} s, {}",
        t0.elapsed().as_secs_f64(),
        done["summary"]
    );
    assert_eq!(done["ok"], true, "{done}");

    // Every output frame against the plan.
    let (ca, cb) = (Clip::read(&a), Clip::read(&b));
    let got = oracle::decode_counters(&out, 640, 360).unwrap();
    assert_eq!(
        got.len() as u64,
        plan.frame_count(FPS),
        "output frame count"
    );
    let (mut plain, mut blend, mut zoom, mut unreadable) = (0, Vec::new(), Vec::new(), 0);
    let mut wrong = Vec::new();
    for (i, g) in got.iter().enumerate() {
        match planned(&plan, i as u64) {
            Planned::Plain(layer, src_ms) => {
                plain += 1;
                let clip = if layer.source == Source::Main {
                    &ca
                } else {
                    &cb
                };
                let want = clip.counters[clip.frame_at(src_ms)];
                match (g, want) {
                    (Some(g), Some(w)) if *g != w => wrong.push((i, *g, w)),
                    (None, _) | (_, None) => unreadable += 1,
                    _ => {}
                }
            }
            Planned::Blend => blend.push(i),
            Planned::Zoom => zoom.push(i),
        }
    }
    println!(
        "{} frames: {plain} plain ({} wrong, {unreadable} unreadable), {} crossfade, {} zoomed; first wrong {:?}",
        got.len(),
        wrong.len(),
        blend.len(),
        zoom.len(),
        &wrong[..wrong.len().min(8)]
    );
    assert_eq!(blend.len(), 30, "500 ms of crossfade at 60 fps");
    assert_eq!(zoom.len(), 60, "1 s zoomed at 60 fps");
    assert!(unreadable <= plain / 50, "{unreadable} unreadable frames");
    assert!(
        wrong.len() <= plain / 50,
        "{} frames off the plan",
        wrong.len()
    );
    // Each side of each join (frame 239 is 3983 ms out = a's 4983 ms).
    assert_eq!(got[239], ca.counters[ca.frame_at(4_983)], "a's last frame");
    assert_eq!(got[240], cb.counters[cb.frame_at(0)], "b from 4 s");
    assert_eq!(
        got[390],
        cb.counters[cb.frame_at(3_500)],
        "b's 3.5 s at 6.5 s: the crossfade starts it 500 ms early"
    );

    // The middle of the crossfade is half of each side; a zoomed frame is the source
    // frame the plan says, doubled about the centre.
    let gray_out = |i: u64| gray_frame(&out, i as usize);
    let source_gray = |l: &Layer, t: u64| {
        let src = l.source_ms(t).unwrap();
        cb.gray(&b, cb.frame_at(src))
    };
    let i = blend[blend.len() / 2] as u64;
    let t = plan.frame_out_ms(i, FPS).unwrap();
    let layers = plan.visible_at(t);
    let (under, over) = (layers[layers.len() - 2], layers[layers.len() - 1]);
    let alpha = f64::from(over.opacity_at(t));
    let (u, o) = (source_gray(under, t), source_gray(over, t));
    let mix: Vec<u8> = u
        .iter()
        .zip(&o)
        .map(|(x, y)| (f64::from(*x) * (1.0 - alpha) + f64::from(*y) * alpha).round() as u8)
        .collect();
    let frame = gray_out(i);
    let (mix_db, under_db, over_db) = (psnr(&frame, &mix), psnr(&frame, &u), psnr(&frame, &o));
    println!(
        "crossfade frame {i} (alpha {alpha:.2}): {mix_db:.1} dB against the mix, {under_db:.1} / {over_db:.1} dB against either side"
    );
    let i = zoom[zoom.len() / 2] as u64;
    let t = plan.frame_out_ms(i, FPS).unwrap();
    let top = *plan.visible_at(t).last().unwrap();
    let src = source_gray(top, t);
    let want = zoomed(&src, 2.0);
    let frame = gray_out(i);
    let (zoom_db, flat_db) = (psnr(&frame, &want), psnr(&frame, &src));
    println!(
        "zoomed frame {i}: {zoom_db:.1} dB against the reference, {flat_db:.1} dB against the plain frame"
    );
    assert!(mix_db >= 35.0, "{mix_db:.1} dB");
    assert!(mix_db > under_db.max(over_db) + 3.0, "not a mix of the two");
    assert!(zoom_db >= 35.0, "{zoom_db:.1} dB");
    assert!(flat_db < zoom_db - 8.0, "not zoomed");

    // The Library lists the export beside its clips.
    events.wait_for(Duration::from_secs(10), |e| {
        e["event"] == "library_changed" && e["reason"] == "exported"
    });
    let list = call(&discovery, "library.list", Value::Null);
    assert_eq!(list["total"], 3, "{list}");
    let key = format!(
        "Scout Test Game/{}",
        out.file_name().unwrap().to_string_lossy()
    );
    let c = list["clips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == key.as_str())
        .unwrap_or_else(|| panic!("{key} not listed: {list}"));
    let ms = c["info"]["duration_ms"].as_u64().unwrap();
    assert!((8_400..=8_600).contains(&ms), "{ms} ms");
    assert_eq!(c["info"]["codec"], "H.264");
    assert_eq!(c["info"]["audio_tracks"], 1);

    // The montage follows a rename of its second clip.
    let r = call(
        &discovery,
        "clip.rename",
        json!({"path": b_s, "name": "Second"}),
    );
    let again = call(&discovery, "editor.project.get", json!({"path": a_s}));
    let renamed = again["clips"].as_array().unwrap();
    assert_eq!(renamed.len(), 1, "{again}");
    assert_eq!(renamed[0]["status"], "ok", "{again}");
    assert_eq!(
        renamed[0]["key"], "Scout Test Game/Second.mp4",
        "{again} {r}"
    );
}
