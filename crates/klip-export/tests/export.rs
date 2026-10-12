//! M8.3 acceptance. The hardware tests (`#[ignore]`, run by `cargo xtask e2e`) make a
//! barcoded clip with the machine's H.264 encoder (two audio tracks with beeps,
//! bookmarks), export it through every path (cuts + speed, text, tall crop, Discord
//! fit and step-down, cancel, volumes) and check the result with ffmpeg as the oracle.
//! `hostile_plans_fail_cleanly` needs nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use klip_capture::audio::{self, CHANNELS, SAMPLE_RATE};
use klip_capture::{CaptureStats, EncodeConfig, PacketSink, Timing, attach_sink_with};
use klip_edit::{
    Composition, FormatKind, Piece, Position, Project, Render, SourceInfo, Speed, TextLayer,
    Volumes, compose,
};
use klip_export::{Encoders, ExportError, ExportRequest, Preset, Progress, export};
use klip_media::{Codec, NANOS_PER_SEC, Packet, TrackInfo};
use klip_mp4::{Marker, Mp4Options, Mp4Track, save_mp4_atomic};
use klip_probe::EncoderChoice;
use klip_testkit::{audio as beeps, barcode, oracle};

const FPS: u32 = 60;
const GAME_BEEPS: [f64; 3] = [0.5, 1.5, 2.5];
const MIC_BEEPS: [f64; 2] = [1.0, 2.0];

/// The audio tracks a clip has, as the recorder writes them.
#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// `Game` + `Microphone` (the tests' usual clip).
    GameMic,
    /// `Mix` (game and voice) + `Game` + `Microphone`: the recorder with a microphone.
    MixGameMic,
    /// `Mix` alone: a recorder writing one track.
    MixOnly,
}

/// What `make_clip` produces.
struct ClipSpec {
    layout: Layout,
    seconds: u32,
    kbps: u32,
    width: usize,
    height: usize,
    game_beeps: Vec<f64>,
    mic_beeps: Vec<f64>,
    /// Bookmarks (chapters), seconds.
    markers: Vec<f64>,
    /// The first frame's counter.
    counter_base: u32,
}

impl ClipSpec {
    /// The audio tracks: name, beep onsets, enabled.
    fn tracks(&self) -> Vec<(&'static str, Vec<f64>, bool)> {
        let mut mix: Vec<f64> = self
            .game_beeps
            .iter()
            .chain(&self.mic_beeps)
            .copied()
            .collect();
        mix.sort_by(f64::total_cmp);
        let game = ("Game", self.game_beeps.clone(), true);
        match self.layout {
            Layout::GameMic => vec![game, ("Microphone", self.mic_beeps.clone(), false)],
            Layout::MixGameMic => vec![
                ("Mix", mix, true),
                ("Game", self.game_beeps.clone(), false),
                ("Microphone", self.mic_beeps.clone(), false),
            ],
            Layout::MixOnly => vec![("Mix", mix, true)],
        }
    }

    fn hd(seconds: u32, kbps: u32) -> ClipSpec {
        ClipSpec {
            layout: Layout::GameMic,
            seconds,
            kbps,
            width: 1280,
            height: 720,
            game_beeps: GAME_BEEPS.to_vec(),
            mic_beeps: MIC_BEEPS.to_vec(),
            markers: vec![],
            counter_base: 0,
        }
    }
}

#[derive(Default)]
struct Collect {
    info: Mutex<Vec<Option<TrackInfo>>>,
    packets: Mutex<Vec<Vec<Packet>>>,
    errors: Mutex<Vec<String>>,
}

impl PacketSink for Collect {
    fn on_track_info(&self, track: usize, info: TrackInfo) {
        let mut v = self.info.lock().unwrap();
        if v.len() <= track {
            v.resize(track + 1, None);
        }
        v[track] = Some(info);
    }
    fn on_packet(&self, track: usize, packet: Packet) {
        let mut v = self.packets.lock().unwrap();
        if v.len() <= track {
            v.resize(track + 1, Vec::new());
        }
        v[track].push(packet);
    }
    fn on_error(&self, message: String) {
        self.errors.lock().unwrap().push(message);
    }
}

/// The machine's best H.264 encoder, or `None` (with a reason) when the test can't run.
fn setup() -> Option<EncoderChoice> {
    klip_probe::init().unwrap();
    if !oracle::available("ffmpeg") || !oracle::available("ffprobe") {
        eprintln!("skipped: needs ffmpeg and ffprobe");
        return None;
    }
    let vendors: Vec<_> = klip_platform::list_gpus()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| !g.software)
        .map(|g| g.vendor)
        .collect();
    match klip_probe::best(Codec::H264, &vendors, None, Duration::from_secs(10)) {
        Ok(c) => {
            println!("encoder: {} ({})", c.element, c.description);
            Some(c)
        }
        Err(e) => {
            eprintln!("skipped: no H.264 encoder: {e}");
            None
        }
    }
}

fn encoders(choice: &EncoderChoice) -> Encoders {
    let mut e = Encoders::default();
    e.push(choice.clone());
    e
}

fn frame_ns(i: u64) -> u64 {
    i * NANOS_PER_SEC as u64 / u64::from(FPS)
}

/// A barcoded clip (counter = frame index, a flash every second) with "Game" and
/// "Microphone" AAC tracks carrying beeps, and bookmarks as chapters.
fn make_clip(encoder: &EncoderChoice, path: &Path, spec: &ClipSpec) {
    let (w, h) = (spec.width, spec.height);
    let pipeline = gst::Pipeline::new();
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "BGRA")
        .field("width", w as i32)
        .field("height", h as i32)
        .field("framerate", gst::Fraction::new(FPS as i32, 1))
        .build();
    let appsrc = gst_app::AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        .block(true)
        .max_bytes((w * h * 4 * 8) as u64)
        .build();
    let upload = gst::ElementFactory::make(if cfg!(windows) {
        "d3d11upload"
    } else {
        "identity"
    })
    .build()
    .unwrap();
    pipeline.add_many([appsrc.upcast_ref(), &upload]).unwrap();
    appsrc.link(&upload).unwrap();
    let cfg = EncodeConfig {
        encoder: encoder.clone(),
        bitrate_kbps: spec.kbps,
        keyframe_interval: FPS,
        max_bitrate_kbps: None,
        leaky_queue: false,
    };
    let chain = klip_capture::build_encode_chain(&pipeline, &cfg, w as u32, h as u32, FPS).unwrap();
    upload.link(&chain.input).unwrap();
    let sink = Arc::new(Collect::default());
    let stats = Arc::new(CaptureStats::default());
    let dyn_sink: Arc<dyn PacketSink> = sink.clone();
    chain.attach(0, dyn_sink.clone(), stats.clone(), Timing::AsIs);
    let delay = audio::aac_encoder().map_or(0, audio::aac_encoder_delay);
    let mut audio_srcs = Vec::new();
    let audio_tracks = spec.tracks();
    for (i, (_, onsets, _)) in audio_tracks.iter().enumerate() {
        let track = i + 1;
        let (input, appsink) = audio::build_aac_chain(&pipeline, 160_000).unwrap();
        appsink.set_property("async", false);
        let acaps = gst::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("layout", "interleaved")
            .field("rate", SAMPLE_RATE as i32)
            .field("channels", CHANNELS as i32)
            .build();
        let src = gst_app::AppSrc::builder()
            .caps(&acaps)
            .format(gst::Format::Time)
            .block(true)
            .max_bytes(1 << 22)
            .build();
        pipeline.add(&src).unwrap();
        src.link(&input).unwrap();
        attach_sink_with(
            &appsink,
            track,
            dyn_sink.clone(),
            stats.clone(),
            Timing::Shift(-delay),
        );
        audio_srcs.push((src, onsets.clone()));
    }
    pipeline.set_state(gst::State::Playing).unwrap();
    let frames = u64::from(spec.seconds * FPS);
    for i in 0..frames {
        let data =
            barcode::render_bgra(spec.counter_base + i as u32, w, h, i % u64::from(FPS) == 0);
        let mut buf = gst::Buffer::from_mut_slice(data);
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(frame_ns(i)));
            b.set_duration(gst::ClockTime::from_nseconds(frame_ns(i + 1) - frame_ns(i)));
        }
        appsrc.push_buffer(buf).unwrap();
    }
    appsrc.end_of_stream().unwrap();
    for (src, onsets) in &audio_srcs {
        let samples = beeps::render_beeps_s16(
            SAMPLE_RATE,
            CHANNELS as u16,
            f64::from(spec.seconds),
            onsets,
        );
        let per_chunk = SAMPLE_RATE as usize * CHANNELS as usize; // one second
        for (n, chunk) in samples.chunks(per_chunk).enumerate() {
            let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut buf = gst::Buffer::from_mut_slice(bytes);
            {
                let b = buf.get_mut().unwrap();
                b.set_pts(gst::ClockTime::from_seconds(n as u64));
                b.set_duration(gst::ClockTime::from_nseconds(
                    (chunk.len() / CHANNELS as usize) as u64 * NANOS_PER_SEC as u64
                        / u64::from(SAMPLE_RATE),
                ));
            }
            src.push_buffer(buf).unwrap();
        }
        src.end_of_stream().unwrap();
    }
    let bus = pipeline.bus().unwrap();
    let msg = bus
        .timed_pop_filtered(
            Some(gst::ClockTime::from_seconds(120)),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("clip encode finished");
    if let gst::MessageView::Error(e) = msg.view() {
        panic!("clip encode failed: {}", e.error());
    }
    pipeline.set_state(gst::State::Null).unwrap();
    assert!(
        sink.errors.lock().unwrap().is_empty(),
        "{:?}",
        sink.errors.lock().unwrap()
    );
    let info = sink.info.lock().unwrap();
    let packets = sink.packets.lock().unwrap();
    assert_eq!(packets[0].len() as u64, frames, "every frame encoded");
    let mut tracks = vec![Mp4Track::new(info[0].as_ref().unwrap(), &packets[0]).named("Video")];
    for (i, (name, _, enabled)) in audio_tracks.iter().enumerate() {
        tracks.push(
            Mp4Track::new(info[i + 1].as_ref().unwrap(), &packets[i + 1])
                .named(name)
                .enabled(*enabled),
        );
    }
    let markers: Vec<Marker> = spec
        .markers
        .iter()
        .enumerate()
        .map(|(i, s)| Marker {
            at: (s * NANOS_PER_SEC as f64).round() as i64,
            title: format!("Bookmark {}", i + 1),
        })
        .collect();
    save_mp4_atomic(
        path,
        &tracks,
        &Mp4Options {
            creation_time_unix: 1_700_000_000,
            force_co64: false,
            markers: &markers,
        },
    )
    .unwrap();
}

fn source_info(path: &Path) -> SourceInfo {
    let mut f = std::fs::File::open(path).unwrap();
    let info = klip_mp4::probe(&mut f).unwrap();
    let v = info.video.unwrap();
    SourceInfo {
        width: v.width,
        height: v.height,
        duration_ms: info.duration_ms.unwrap(),
        fps: v.fps,
        bookmarks_ms: info.chapters_ms,
        events: Vec::new(),
    }
}

/// The Normal-mode plan (what the expectations are computed from) and the composition
/// the renderer gets for the same project.
fn plan(p: &Project, info: &SourceInfo) -> (Render, Composition) {
    (Render::new(p, info), compose(p, info))
}

fn request<'a>(
    preset: Preset,
    clip: &'a Path,
    composition: &'a Composition,
    out: &'a Path,
    encoders: &'a Encoders,
) -> ExportRequest<'a> {
    ExportRequest {
        source: clip,
        composition,
        media: &[],
        preset,
        out,
        encoders,
        discord_limit_bytes: klip_export::DISCORD_LIMIT_BYTES,
        pretend_overshoot: 0.0,
    }
}

fn run(req: &ExportRequest<'_>) -> klip_export::ExportSummary {
    export(req, &mut |_| {}, &AtomicBool::new(false)).unwrap()
}

/// The counter output frame `i` must show: the source frame with the largest
/// presentation time (floor ms) at or before `frame_source_ms(i)`.
fn expected_counter(render: &Render, i: u64, fps: u32) -> u32 {
    let src_ms = render.frame_source_ms(i, fps).unwrap();
    let mut k = src_ms * u64::from(FPS) / 1000;
    while (k + 1) * 1000 / u64::from(FPS) <= src_ms {
        k += 1;
    }
    k as u32
}

/// The same thing in closed form, independent of the plan's math: pieces that start on
/// a frame boundary show `base + 2j` (2x), `base + j` (1x), `base + j/2` (0.5x) at
/// their `j`-th output frame.
fn closed_form_counter(pieces: &[Piece], i: u64) -> u32 {
    let mut out_frame = 0u64;
    for p in pieces {
        let src_frames = (p.end_ms - p.start_ms) * u64::from(FPS) / 1000;
        let out_frames = match p.speed {
            Speed::Double => src_frames / 2,
            Speed::Normal => src_frames,
            Speed::Half => src_frames * 2,
        };
        if i < out_frame + out_frames {
            let j = i - out_frame;
            let base = p.start_ms * u64::from(FPS) / 1000;
            return (base
                + match p.speed {
                    Speed::Double => 2 * j,
                    Speed::Normal => j,
                    Speed::Half => j / 2,
                }) as u32;
        }
        out_frame += out_frames;
    }
    panic!("frame {i} past the plan");
}

fn onset_times(path: &Path) -> Vec<f64> {
    let samples = oracle::decode_audio_mono(path, 0, SAMPLE_RATE).unwrap();
    beeps::onsets(&samples, SAMPLE_RATE, 0.1, 0.2)
}

fn close(times: &[f64], want: &[f64], tol: f64) -> bool {
    times.len() == want.len() && times.iter().zip(want).all(|(a, b)| (a - b).abs() < tol)
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn cuts_and_speed_map_every_output_frame() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    let mut spec = ClipSpec::hd(20, 30_000);
    // Beeps inside each piece (2x, 1x, 0.5x) and bookmarks: one in the cut part.
    spec.game_beeps = vec![3.0, 9.0, 12.5];
    spec.mic_beeps = vec![];
    spec.markers = vec![1.0, 4.0, 9.0, 12.5];
    make_clip(&enc, &clip, &spec);
    let pieces = vec![
        Piece {
            start_ms: 2_000,
            end_ms: 6_000,
            speed: Speed::Double,
        },
        Piece {
            start_ms: 8_000,
            end_ms: 10_000,
            speed: Speed::Normal,
        },
        Piece {
            start_ms: 12_000,
            end_ms: 14_000,
            speed: Speed::Half,
        },
    ];
    let mut p = Project::new(20_000);
    p.pieces = pieces.clone();
    let info = source_info(&clip);
    assert_eq!(info.bookmarks_ms, vec![1_000, 4_000, 9_000, 12_500]);
    let (render, comp) = plan(&p, &info);
    assert_eq!(render.output_ms, 8_000);
    let out = dir.path().join("clip - Original.mp4");
    let encoders = encoders(&enc);
    let t = Instant::now();
    let summary = run(&request(Preset::Original, &clip, &comp, &out, &encoders));
    println!(
        "export (trim + cut + 2x + 0.5x, 8 s of 720p60): {} frames in {:.1} s, {} bytes, {}",
        summary.frames,
        t.elapsed().as_secs_f64(),
        summary.bytes,
        summary.encoder
    );
    assert_eq!(summary.frames, render.frame_count(FPS));
    let counters = oracle::decode_counters(&out, 640, 360).unwrap();
    assert_eq!(
        counters.len() as u64,
        render.frame_count(FPS),
        "output frame count"
    );
    let mut bad = Vec::new();
    for (i, c) in counters.iter().enumerate() {
        let want = expected_counter(&render, i as u64, FPS);
        // The plan floors output times to whole ms, so a 2x piece can trail the ideal
        // 1/60 s math by one source frame; never more, never ahead.
        let ideal = closed_form_counter(&pieces, i as u64);
        assert!(
            want <= ideal && ideal - want <= 1,
            "frame {i}: plan {want} vs closed form {ideal}"
        );
        if *c != Some(want) {
            bad.push((i, *c, want));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} frames show the wrong source frame; first: {:?}",
        bad.len(),
        counters.len(),
        &bad[..bad.len().min(12)]
    );
    let streams = oracle::probe_streams(&out).unwrap();
    let video = streams.iter().find(|s| s.width.is_some()).unwrap();
    assert!(
        (video.avg_fps().unwrap() - 60.0).abs() < 0.5,
        "{:?}",
        video.avg_fps()
    );
    assert!(
        (video.duration_secs().unwrap() - 8.0).abs() < 0.05,
        "{:?}",
        video.duration_secs()
    );
    let audio_streams: Vec<_> = streams.iter().filter(|s| s.sample_rate.is_some()).collect();
    assert_eq!(audio_streams.len(), 1, "one Mix track");
    assert!((audio_streams[0].duration_secs().unwrap() - 8.0).abs() < 0.1);
    let boxes = oracle::top_level_boxes(&out).unwrap();
    assert!(
        boxes.iter().position(|b| b == "moov") < boxes.iter().position(|b| b == "mdat"),
        "faststart"
    );
    // Audio follows the cuts and the speed: 3.0 s in the 2x piece → 0.5 s out; 9.0 in
    // the 1x piece (out 2–4 s) → 3.0; 12.5 in the 0.5x piece (out 4–8 s) → 5.0.
    let times = onset_times(&out);
    println!("beeps after cuts: {times:?}");
    assert!(close(&times, &[0.5, 3.0, 5.0], 0.03), "{times:?}");
    // Bookmarks: the one at 1.0 s is cut; 4.0 → 1.0 (2x), 9.0 → 3.0, 12.5 → 5.0.
    let chapters = oracle::chapters(&out).unwrap();
    println!("chapters: {chapters:?}");
    assert_eq!(chapters.len(), 3, "{chapters:?}");
    for ((at, title), (want, n)) in chapters.iter().zip([(1.0, 1), (3.0, 2), (5.0, 3)]) {
        assert!((at - want).abs() < 0.002, "{chapters:?}");
        assert_eq!(title, &format!("Bookmark {n}"));
    }
    // A plan that claims not to know the clip's length and reaches past it is refused
    // (nothing is encoded for hours).
    let mut past = compose(&p, &info);
    past.source.duration_ms = 0;
    past.layers[2].src_out_ms = 60_000;
    past.duration_ms = past.layers[2].end_ms + (60_000 - 14_000) * 2;
    past.layers[2].end_ms = past.duration_ms;
    let r = export(
        &request(
            Preset::Original,
            &clip,
            &past,
            &dir.path().join("past.mp4"),
            &encoders,
        ),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(matches!(r, Err(ExportError::Plan(_))), "{r:?}");
}

fn luma(p: &[u8; 4]) -> f64 {
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
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

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn text_overlay_matches_the_bitmap() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    make_clip(&enc, &clip, &ClipSpec::hd(5, 40_000));
    let mut p = Project::new(5_000);
    p.text = vec![TextLayer {
        id: 1,
        preset: "bold".into(),
        text: "GG WP no way".into(),
        position: Position::Bottom,
        start_ms: 1_000,
        end_ms: 3_000,
    }];
    let (render, comp) = plan(&p, &source_info(&clip));
    let out = dir.path().join("clip - Original.mp4");
    run(&request(
        Preset::Original,
        &clip,
        &comp,
        &out,
        &encoders(&enc),
    ));
    let (w, h) = (1280usize, 720usize);
    let bitmap = klip_edit::render_layer(&render.texts[0], w as u32, h as u32).unwrap();
    let got = oracle::decode_gray_frame(&out, 2.5, w, h).unwrap();
    let src = oracle::decode_gray_frame(&clip, 2.5, w, h).unwrap();
    let (bx, by, bw, bh) = (
        bitmap.x as usize,
        bitmap.y as usize,
        bitmap.width as usize,
        bitmap.height as usize,
    );
    let mut expected = Vec::with_capacity(bw * bh);
    let mut actual = Vec::with_capacity(bw * bh);
    let mut plain = Vec::with_capacity(bw * bh);
    for y in 0..bh {
        for x in 0..bw {
            let px = bitmap.pixel(x as u32, y as u32).unwrap();
            let a = f64::from(px[3]) / 255.0;
            let under = f64::from(src[(by + y) * w + bx + x]);
            expected.push((luma(&px) * a + under * (1.0 - a)).round() as u8);
            actual.push(got[(by + y) * w + bx + x]);
            plain.push(src[(by + y) * w + bx + x]);
        }
    }
    let overlay = psnr(&expected, &actual);
    let without = psnr(&plain, &actual);
    println!(
        "overlay region {bw}x{bh} at {bx},{by}: PSNR {overlay:.1} dB vs expected, {without:.1} dB vs no text"
    );
    assert!(overlay >= 40.0, "overlay PSNR {overlay:.1} dB");
    assert!(
        without < overlay - 5.0,
        "the text is not visible: {without:.1} dB"
    );
    // Outside the cue the frame is untouched.
    let before = oracle::decode_gray_frame(&out, 0.5, w, h).unwrap();
    let src_before = oracle::decode_gray_frame(&clip, 0.5, w, h).unwrap();
    assert!(psnr(&before, &src_before) >= 38.0);
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn tall_crop_keeps_the_right_columns() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    make_clip(&enc, &clip, &ClipSpec::hd(4, 30_000));
    let encoders = encoders(&enc);
    let (w, h) = (1280usize, 720usize);
    let src = oracle::decode_gray_frame(&clip, 2.5, w, h).unwrap();
    for crop_x in [0.0f32, 0.5, 1.0] {
        let mut p = Project::new(4_000);
        p.format.kind = FormatKind::Tall;
        p.format.crop_x = crop_x;
        let (render, comp) = plan(&p, &source_info(&clip));
        let (cw, ch, cx) = (
            render.crop.width as usize,
            render.crop.height as usize,
            render.crop.x as usize,
        );
        assert_eq!((cw, ch), (404, 720));
        let out = dir.path().join(format!("tall-{crop_x}.mp4"));
        let summary = run(&request(Preset::TikTok, &clip, &comp, &out, &encoders));
        assert_eq!((summary.width, summary.height), (404, 720));
        let streams = oracle::probe_streams(&out).unwrap();
        let video = streams.iter().find(|s| s.width.is_some()).unwrap();
        assert_eq!((video.width, video.height), (Some(404), Some(720)));
        let got = oracle::decode_gray_frame(&out, 2.5, cw, ch).unwrap();
        let expected: Vec<u8> = (0..ch)
            .flat_map(|y| src[y * w + cx..y * w + cx + cw].iter().copied())
            .collect();
        let q = psnr(&expected, &got);
        // The band (top 14 %) is a different crop of the same barcode: compare below it.
        let below = ch * 15 / 100;
        let q_below = psnr(&expected[below * cw..], &got[below * cw..]);
        println!(
            "tall crop at x={cx} ({crop_x}): PSNR {q:.1} dB whole, {q_below:.1} dB below the band"
        );
        assert!(q_below >= 35.0, "crop_x {crop_x}: {q_below:.1} dB");
        // A wrong offset would match the scrolling checker and the moving bar badly.
        if crop_x != 0.5 {
            let other = w / 2 - cw / 2;
            let wrong: Vec<u8> = (below..ch)
                .flat_map(|y| src[y * w + other..y * w + other + cw].iter().copied())
                .collect();
            assert!(psnr(&wrong, &got[below * cw..]) < q_below - 3.0);
        }
    }
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn discord_preset_fits_the_limit() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let encoders = encoders(&enc);
    let limit = klip_export::DISCORD_LIMIT_BYTES;
    for (seconds, overshoot) in [(5u32, 0.0), (12, 0.0), (25, 4.0), (40, 0.0), (60, 0.0)] {
        let clip = dir.path().join(format!("clip{seconds}.mp4"));
        make_clip(&enc, &clip, &ClipSpec::hd(seconds, 30_000));
        let (render, comp) = plan(
            &Project::new(u64::from(seconds) * 1000),
            &source_info(&clip),
        );
        let out = dir.path().join(format!("clip{seconds} - Discord.mp4"));
        let mut req = request(Preset::Discord, &clip, &comp, &out, &encoders);
        req.pretend_overshoot = overshoot;
        let t = Instant::now();
        let summary = export(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        println!(
            "discord {seconds} s: {} bytes ({:.1} % of the limit), {}x{}@{} {} kbit/s, {} pass(es), {:.1} s",
            summary.bytes,
            summary.bytes as f64 * 100.0 / limit as f64,
            summary.width,
            summary.height,
            summary.fps,
            summary.bitrate_kbps,
            summary.passes,
            t.elapsed().as_secs_f64()
        );
        assert!(
            summary.bytes <= limit,
            "{seconds} s: {} > {limit}",
            summary.bytes
        );
        assert_eq!(std::fs::metadata(&out).unwrap().len(), summary.bytes);
        if overshoot > 1.0 {
            assert!(summary.passes >= 2, "the forced second pass ran");
        }
        let streams = oracle::probe_streams(&out).unwrap();
        let video = streams.iter().find(|s| s.width.is_some()).unwrap();
        assert_eq!(video.codec_name.as_deref(), Some("h264"));
        assert!((video.duration_secs().unwrap() - f64::from(seconds)).abs() < 0.05);
        assert_eq!(
            (video.width, video.height),
            (Some(summary.width), Some(summary.height))
        );
        assert!((video.avg_fps().unwrap() - f64::from(summary.fps)).abs() < 0.5);
        let audio = streams.iter().find(|s| s.sample_rate.is_some()).unwrap();
        assert_eq!(audio.codec_name.as_deref(), Some("aac"));
        let counters = oracle::decode_counters(&out, 640, 360).unwrap();
        assert_eq!(counters.len() as u64, render.frame_count(summary.fps));
        let report = klip_testkit::continuity::analyze(&counters);
        assert_eq!(report.unreadable, 0, "{report}");
        assert_eq!(report.backwards, 0, "{report}");
        if seconds == 60 {
            // first_pass: (0.94 x 10 MB - audio) / 60 s = 1125 kbit/s → 720p at 30 fps.
            assert_eq!((summary.height, summary.fps), (720, 30));
        }
    }
    // A tight limit on a 1080p clip steps the picture down to 480p30 and scales.
    let clip = dir.path().join("clip1080.mp4");
    let mut spec = ClipSpec::hd(5, 30_000);
    spec.width = 1920;
    spec.height = 1080;
    make_clip(&enc, &clip, &spec);
    let (render, comp) = plan(&Project::new(5_000), &source_info(&clip));
    let out = dir.path().join("clip1080 - Discord.mp4");
    let mut req = request(Preset::Discord, &clip, &comp, &out, &encoders);
    req.discord_limit_bytes = 400_000;
    let summary = export(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    println!(
        "discord 1080p at 400 kB: {} bytes, {}x{}@{} {} kbit/s, {} pass(es)",
        summary.bytes,
        summary.width,
        summary.height,
        summary.fps,
        summary.bitrate_kbps,
        summary.passes
    );
    assert!(summary.bytes <= 400_000);
    assert_eq!((summary.width, summary.height, summary.fps), (852, 480, 30));
    let streams = oracle::probe_streams(&out).unwrap();
    let video = streams.iter().find(|s| s.width.is_some()).unwrap();
    assert_eq!((video.width, video.height), (Some(852), Some(480)));
    let counters = oracle::decode_counters(&out, 640, 360).unwrap();
    assert_eq!(counters.len() as u64, render.frame_count(30));
    // 30 fps from a 60 fps source: every other frame.
    for (i, c) in counters.iter().enumerate() {
        // The movie outlasts the last video frame by a few ms: the last frame is held.
        let want = expected_counter(&render, i as u64, 30).min(5 * FPS - 1);
        assert_eq!(*c, Some(want), "frame {i}");
    }
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn cancel_leaves_no_file() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    make_clip(&enc, &clip, &ClipSpec::hd(10, 20_000));
    let (render, comp) = plan(&Project::new(10_000), &source_info(&clip));
    let out = dir.path().join("clip - YouTube.mp4");
    let cancel = AtomicBool::new(false);
    let seen = Mutex::new(Vec::new());
    let r = export(
        &request(Preset::YouTube, &clip, &comp, &out, &encoders(&enc)),
        &mut |p: Progress| {
            seen.lock().unwrap().push(p);
            if p.frames_done >= 60 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    );
    assert!(matches!(r, Err(ExportError::Cancelled)), "{r:?}");
    assert!(!out.exists());
    let leftovers: Vec<PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p != &clip)
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|p| p.frames_total == render.frame_count(FPS)),
        "{} reports, first {:?}",
        seen.len(),
        seen.first()
    );
    assert!(
        seen.windows(2)
            .all(|w| w[0].frames_done <= w[1].frames_done)
    );
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn volumes_pick_the_tracks() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    make_clip(&enc, &clip, &ClipSpec::hd(4, 20_000));
    let encoders = encoders(&enc);
    for (name, volumes, want) in [
        (
            "game only",
            Volumes {
                game: 1.0,
                mic: 0.0,
            },
            &GAME_BEEPS[..],
        ),
        (
            "mic only",
            Volumes {
                game: 0.0,
                mic: 1.0,
            },
            &MIC_BEEPS[..],
        ),
    ] {
        let mut p = Project::new(4_000);
        p.volumes = volumes;
        let (_, comp) = plan(&p, &source_info(&clip));
        let out = dir.path().join(format!("{name}.mp4"));
        run(&request(Preset::Original, &clip, &comp, &out, &encoders));
        let times = onset_times(&out);
        println!("{name}: beeps at {times:?}");
        assert!(close(&times, want, 0.005), "{name}: {times:?} vs {want:?}");
    }
    // 2x halves the beep times, 0.5x doubles them (scaletempo keeps the pitch; the
    // onsets are what we can check).
    for (speed, factor, name) in [(Speed::Double, 0.5, "double"), (Speed::Half, 2.0, "half")] {
        let mut p = Project::new(4_000);
        p.pieces = vec![Piece {
            start_ms: 0,
            end_ms: 4_000,
            speed,
        }];
        p.volumes = Volumes {
            game: 1.0,
            mic: 0.0,
        };
        let (_, comp) = plan(&p, &source_info(&clip));
        let out = dir.path().join(format!("{name}.mp4"));
        run(&request(Preset::Original, &clip, &comp, &out, &encoders));
        let times = onset_times(&out);
        println!("{name}: beeps at {times:?}");
        let want: Vec<f64> = GAME_BEEPS.iter().map(|t| t * factor).collect();
        // A stretched attack is detected early, by a varying part of a scaletempo
        // window: 0.5x onsets came 30–52 ms early over runs (2026-10-10): 70 ms.
        assert!(close(&times, &want, 0.07), "{name}: {times:?} vs {want:?}");
    }
}

/// IMPROVEMENTS G4: the recorder's own layouts. With `Mix` + `Game` + `Microphone`, the
/// Mix must stay out of the export (else the game beeps come twice and the voice can't be
/// turned down); a clip with `Mix` alone follows the game slider.
#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn recorder_track_layouts_mix_the_right_tracks() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let encoders = encoders(&enc);
    let mut all: Vec<f64> = GAME_BEEPS.iter().chain(&MIC_BEEPS).copied().collect();
    all.sort_by(f64::total_cmp);
    for (layout, game, mic, want) in [
        (Layout::MixGameMic, 1.0, 0.0, GAME_BEEPS.to_vec()),
        (Layout::MixGameMic, 0.0, 1.0, MIC_BEEPS.to_vec()),
        (Layout::MixOnly, 1.0, 0.0, all.clone()),
        (Layout::MixOnly, 0.0, 1.0, vec![]),
    ] {
        let name = format!(
            "{}-g{game}-m{mic}",
            if layout == Layout::MixOnly {
                "mix"
            } else {
                "mix+game+mic"
            }
        );
        let clip = dir.path().join(format!("{name}-clip.mp4"));
        let mut spec = ClipSpec::hd(4, 20_000);
        spec.layout = layout;
        make_clip(&enc, &clip, &spec);
        let mut p = Project::new(4_000);
        p.volumes = Volumes { game, mic };
        let (_, comp) = plan(&p, &source_info(&clip));
        let out = dir.path().join(format!("{name}.mp4"));
        run(&request(Preset::Original, &clip, &comp, &out, &encoders));
        let times = onset_times(&out);
        println!("{name}: beeps at {times:?}");
        assert!(close(&times, &want, 0.005), "{name}: {times:?} vs {want:?}");
    }
}

#[test]
fn hostile_plans_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let encoders = Encoders::default();
    let missing = dir.path().join("nope.mp4");
    let out = dir.path().join("out.mp4");
    let bad = Composition::default();
    let r = export(
        &request(Preset::Original, &missing, &bad, &out, &encoders),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(matches!(r, Err(ExportError::Plan(_))), "{r:?}");
    let src = SourceInfo {
        width: 64,
        height: 64,
        duration_ms: 1_000,
        fps: Some(30.0),
        bookmarks_ms: vec![],
        events: Vec::new(),
    };
    let mut wide = compose(&Project::new(1_000), &src);
    wide.crop.width = 10_000;
    let r = export(
        &request(Preset::Original, &missing, &wide, &out, &encoders),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(matches!(r, Err(ExportError::Plan(_))), "{r:?}");
    let ok = compose(&Project::new(1_000), &src);
    let r = export(
        &request(Preset::Original, &missing, &ok, &out, &encoders),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(matches!(r, Err(ExportError::Io(_))), "{r:?}");
    let junk = dir.path().join("junk.mp4");
    std::fs::write(&junk, vec![0x42u8; 300]).unwrap();
    let r = export(
        &request(Preset::Original, &junk, &ok, &out, &encoders),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(matches!(r, Err(ExportError::Source(_))), "{r:?}");
    std::fs::write(&out, b"taken").unwrap();
    let r = export(
        &request(Preset::Original, &junk, &ok, &out, &encoders),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(
        matches!(
            r,
            Err(ExportError::Mp4(klip_mp4::Mp4Error::AlreadyExists(_)))
        ),
        "{r:?}"
    );
    assert!(!dir.path().join("out.mp4.part").exists());
}

/// Writes a 12 s barcoded clip (beeps, bookmarks) to `$KLIP_DIAG_CLIP` for driving the
/// app by hand: `KLIP_DIAG_CLIP=<path> cargo test -p klip-export --release --test export
/// -- --ignored diag_make_clip`.
#[test]
#[ignore = "diagnostic: writes a clip where KLIP_DIAG_CLIP says"]
fn diag_make_clip() {
    let Some(target) = std::env::var_os("KLIP_DIAG_CLIP") else {
        eprintln!("skipped: set KLIP_DIAG_CLIP");
        return;
    };
    let Some(enc) = setup() else { return };
    let path = PathBuf::from(target);
    let mut spec = ClipSpec::hd(12, 20_000);
    spec.markers = vec![2.0, 7.5];
    make_clip(&enc, &path, &spec);
    println!("wrote {}", path.display());
}

/// M9.1 spike (ADR 0018): can a compositor pipeline carry Advanced mode? Two 1080p60
/// clips, the second zoomed by a keyframe (1x → 2x over 10 s) and crossfaded in over
/// 1 s, composed by `compositor` (system memory; and `d3d11compositor` where it works)
/// into the recorder's encode chain. Prints the throughput; the ADR wants ≥ 2x realtime.
/// Not part of `cargo xtask e2e` (a diagnostic): `cargo test -p klip-export --release
/// --test export -- --ignored diag_compositor_spike --nocapture`.
#[test]
#[ignore = "diagnostic: compositor throughput for ADR 0018"]
fn diag_compositor_spike() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let seconds = 10u32;
    let mut spec = ClipSpec::hd(seconds, 20_000);
    spec.width = 1920;
    spec.height = 1080;
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    make_clip(&enc, &a, &spec);
    make_clip(&enc, &b, &spec);
    for variant in ["compositor", "d3d11compositor"] {
        match compose_spike(&enc, &a, &b, seconds, variant) {
            Ok((elapsed, frames)) => println!(
                "{variant}: {frames} frames of 1920x1080@60 in {:.2} s = {:.1}x realtime",
                elapsed.as_secs_f64(),
                f64::from(seconds) / elapsed.as_secs_f64()
            ),
            Err(e) => println!("{variant}: not usable here: {e}"),
        }
    }
}

fn compose_spike(
    encoder: &EncoderChoice,
    a: &Path,
    b: &Path,
    seconds: u32,
    variant: &str,
) -> Result<(Duration, usize), String> {
    let (w, h) = (1920i32, 1080i32);
    let pipeline = gst::Pipeline::new();
    let comp = gst::ElementFactory::make(variant)
        .build()
        .map_err(|e| e.to_string())?;
    let d3d = variant == "d3d11compositor";
    let out_caps = gst::ElementFactory::make("capsfilter").build().unwrap();
    let caps = if d3d {
        gst::Caps::from_str(&format!(
            "video/x-raw(memory:D3D11Memory),width={w},height={h},framerate=60/1"
        ))
        .unwrap()
    } else {
        gst::Caps::from_str(&format!(
            "video/x-raw,format=NV12,width={w},height={h},framerate=60/1"
        ))
        .unwrap()
    };
    out_caps.set_property("caps", &caps);
    pipeline.add_many([&comp, &out_caps]).unwrap();
    comp.link(&out_caps).map_err(|e| e.to_string())?;
    let cfg = EncodeConfig {
        encoder: encoder.clone(),
        bitrate_kbps: 20_000,
        keyframe_interval: FPS,
        max_bitrate_kbps: None,
        leaky_queue: false,
    };
    let chain = klip_capture::build_encode_chain(&pipeline, &cfg, w as u32, h as u32, FPS)
        .map_err(|e| e.to_string())?;
    if d3d {
        out_caps.link(&chain.input).map_err(|e| e.to_string())?;
    } else {
        let up = gst::ElementFactory::make("d3d11upload")
            .build()
            .map_err(|e| e.to_string())?;
        pipeline.add(&up).unwrap();
        out_caps.link(&up).map_err(|e| e.to_string())?;
        up.link(&chain.input).map_err(|e| e.to_string())?;
    }
    let sink = Arc::new(Collect::default());
    let stats = Arc::new(CaptureStats::default());
    let dyn_sink: Arc<dyn PacketSink> = sink.clone();
    chain.attach(0, dyn_sink, stats, Timing::AsIs);

    // Two sources: decodebin → queue → convert(+upload) → compositor pad.
    for (i, path) in [a, b].into_iter().enumerate() {
        let src = gst::ElementFactory::make("filesrc")
            .property("location", path.to_string_lossy().as_ref())
            .build()
            .unwrap();
        let dec = gst::ElementFactory::make("decodebin").build().unwrap();
        let queue = gst::ElementFactory::make("queue").build().unwrap();
        let conv = gst::ElementFactory::make("videoconvert").build().unwrap();
        let tail: gst::Element = if d3d {
            gst::ElementFactory::make("d3d11upload").build().unwrap()
        } else {
            gst::ElementFactory::make("identity").build().unwrap()
        };
        pipeline
            .add_many([&src, &dec, &queue, &conv, &tail])
            .unwrap();
        src.link(&dec).unwrap();
        gst::Element::link_many([&queue, &conv, &tail]).unwrap();
        let pad = comp
            .request_pad_simple("sink_%u")
            .ok_or("no compositor pad")?;
        tail.static_pad("src")
            .unwrap()
            .link(&pad)
            .map_err(|e| format!("{e:?}"))?;
        pad.set_property("zorder", i as u32);
        if i == 1 {
            // Keyframes: zoom 1x → 2x over the clip (centred), alpha 0 → 1 over 4–5 s.
            let total_ns = u64::from(seconds) * NANOS_PER_SEC as u64;
            pad.set_property("alpha", 0.0f64);
            let comp_pad = pad.clone();
            tail.static_pad("src")
                .unwrap()
                .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if let Some(gst::PadProbeData::Buffer(buf)) = &info.data
                        && let Some(pts) = buf.pts()
                    {
                        let t = pts.nseconds() as f64 / total_ns as f64;
                        let zoom = 1.0 + t;
                        let zw = (f64::from(w) * zoom) as i32;
                        let zh = (f64::from(h) * zoom) as i32;
                        comp_pad.set_property("width", zw);
                        comp_pad.set_property("height", zh);
                        comp_pad.set_property("xpos", (w - zw) / 2);
                        comp_pad.set_property("ypos", (h - zh) / 2);
                        let s = pts.seconds_f64();
                        comp_pad.set_property("alpha", (s - 4.0).clamp(0.0, 1.0));
                    }
                    gst::PadProbeReturn::Ok
                });
        }
        let qsink = queue.static_pad("sink").unwrap();
        dec.connect_pad_added(move |_, p| {
            if p.current_caps()
                .and_then(|c| c.structure(0).map(|s| s.name().starts_with("video/")))
                .unwrap_or(false)
            {
                let _ = p.link(&qsink);
            }
        });
    }
    let started = Instant::now();
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;
    let bus = pipeline.bus().unwrap();
    let msg = bus
        .timed_pop_filtered(
            Some(gst::ClockTime::from_seconds(120)),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .ok_or("no EOS within 120 s")?;
    let elapsed = started.elapsed();
    let r = match msg.view() {
        gst::MessageView::Error(e) => Err(format!("{} ({:?})", e.error(), e.debug())),
        _ => Ok(()),
    };
    pipeline.set_state(gst::State::Null).unwrap();
    r?;
    let frames = sink.packets.lock().unwrap().first().map_or(0, Vec::len);
    if frames < usize::try_from(seconds * FPS).unwrap() * 9 / 10 {
        return Err(format!("only {frames} frames came out"));
    }
    Ok((elapsed, frames))
}

// ---------------------------------------------------------------------------------
// M9.3: the composition renderer (montages, transitions, zoom, ducking, Custom).

use klip_edit::advanced::{
    Ease, PlacedClip, Source as Media, Track, TrackKind, TransformKey, Transition, TransitionKind,
};
use klip_edit::compose::{Layer, LayerKind};

/// Counters of the second clip start here, so a frame says which clip it came from.
const B_BASE: u32 = 10_000;

/// The source frame shown at `src_ms`: the largest pts (floor ms) at or before it.
fn counter_at(src_ms: u64) -> u32 {
    let mut k = src_ms * u64::from(FPS) / 1000;
    while (k + 1) * 1000 / u64::from(FPS) <= src_ms {
        k += 1;
    }
    k as u32
}

/// What output frame `i` must show when its top layer is fully opaque (`None` while
/// layers blend): that layer's clip base plus its source frame (a clip of `frames`
/// frames holds its last one past its end: the movie is a few ms longer).
fn composed_counter(c: &Composition, i: u64, fps: u32, frames: u32) -> Option<u32> {
    let t = c.frame_out_ms(i, fps)?;
    let visible = c.visible_at(t);
    let top: &Layer = visible.last()?;
    // Blending, or a dip's black (no counter to read).
    if top.opacity_at(t) < 1.0 || top.kind == LayerKind::Black {
        return None;
    }
    let base = if top.source == Media::Main { 0 } else { B_BASE };
    Some(base + counter_at(top.source_ms(t)?).min(frames - 1))
}

fn library(key: &str) -> Media {
    Media::Library { key: key.into() }
}

fn placed(id: u32, source: Media, in_ms: u64, out_ms: u64, at_ms: u64) -> PlacedClip {
    PlacedClip {
        id,
        source,
        in_ms,
        out_ms,
        at_ms,
        ..PlacedClip::default()
    }
}

fn track(id: u32, kind: TrackKind, clips: Vec<PlacedClip>) -> Track {
    Track {
        id,
        kind,
        name: format!("track {id}"),
        muted: false,
        hidden: false,
        main: false,
        locked: false,
        clips,
    }
}

/// A 48 kHz stereo 16-bit WAV of `f(t)` (−1…1) for `seconds`.
fn write_wav(path: &Path, seconds: f64, f: impl Fn(f64) -> f64) {
    let frames = (seconds * 48_000.0) as u32;
    let mut data = Vec::with_capacity(frames as usize * 4);
    for n in 0..frames {
        let v = (f(f64::from(n) / 48_000.0).clamp(-1.0, 1.0) * 32_767.0) as i16;
        data.extend_from_slice(&v.to_le_bytes());
        data.extend_from_slice(&v.to_le_bytes());
    }
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&48_000u32.to_le_bytes());
    wav.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(path, wav).unwrap();
}

/// The amplitude of `freq` in `samples` (Goertzel).
fn tone_level(samples: &[i16], rate: u32, freq: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * freq / f64::from(rate);
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = f64::from(x) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    2.0 * power.max(0.0).sqrt() / samples.len().max(1) as f64
}

fn window(samples: &[i16], from_s: f64, to_s: f64) -> &[i16] {
    let a = (from_s * f64::from(SAMPLE_RATE)) as usize;
    let b = (to_s * f64::from(SAMPLE_RATE)) as usize;
    &samples[a.min(samples.len())..b.min(samples.len())]
}

fn montage_request<'a>(
    preset: Preset,
    clip: &'a Path,
    composition: &'a Composition,
    media: &'a [(Media, PathBuf)],
    out: &'a Path,
    encoders: &'a Encoders,
) -> ExportRequest<'a> {
    ExportRequest {
        media,
        ..request(preset, clip, composition, out, encoders)
    }
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn montage_crossfade_maps_every_frame() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(4, 8_000));
    make_clip(
        &enc,
        &b,
        &ClipSpec {
            counter_base: B_BASE,
            ..ClipSpec::hd(4, 8_000)
        },
    );
    // Main 0–4 s; above it b's 0–1.5 s at 1 s, then b's 2–3.5 s crossfading in over
    // 500 ms (it starts at 2 s), then the main clip again from 3.5 s.
    let mut p = Project::new(4_000);
    p.advanced.tracks = vec![track(
        1,
        TrackKind::Video,
        vec![
            placed(1, library("b.mp4"), 0, 1_500, 1_000),
            placed(2, library("b.mp4"), 2_000, 3_500, 2_500),
        ],
    )];
    p.advanced.transitions = vec![Transition {
        track: 1,
        after_clip: 1,
        kind: TransitionKind::Crossfade,
        ms: 500,
    }];
    let comp = compose(&p, &source_info(&a));
    // The clip's own length (its movie runs a few ms past the last frame).
    assert!(
        (4_000..4_050).contains(&comp.duration_ms),
        "{}",
        comp.duration_ms
    );
    let media = vec![(library("b.mp4"), b.clone())];
    let out = dir.path().join("montage.mp4");
    let encoders = encoders(&enc);
    let t = Instant::now();
    let summary = run(&montage_request(
        Preset::Original,
        &a,
        &comp,
        &media,
        &out,
        &encoders,
    ));
    println!(
        "montage (4 s of 720p60, two sources, a crossfade): {} frames in {:.1} s",
        summary.frames,
        t.elapsed().as_secs_f64()
    );
    let counters = oracle::decode_counters(&out, 640, 360).unwrap();
    assert_eq!(counters.len() as u64, comp.frame_count(FPS));
    let (mut checked, mut blended, mut bad) = (0, 0, Vec::new());
    for (i, got) in counters.iter().enumerate() {
        match composed_counter(&comp, i as u64, FPS, 4 * FPS) {
            Some(want) => {
                checked += 1;
                if *got != Some(want) {
                    bad.push((i, *got, want));
                }
            }
            None => blended += 1,
        }
    }
    println!(
        "{checked} frames checked, {blended} blended, {} wrong",
        bad.len()
    );
    assert!(
        bad.is_empty(),
        "frames off the plan: {:?}",
        &bad[..bad.len().min(10)]
    );
    assert_eq!(blended, 30, "500 ms of crossfade at 60 fps");
    // Each side of the transition is there.
    assert_eq!(counters[59], Some(59), "main before");
    assert_eq!(counters[60], Some(B_BASE), "b from 1 s");
    assert_eq!(
        counters[150],
        Some(B_BASE + 150),
        "b's 2.5 s at 2.5 s: the crossfade starts it 500 ms early"
    );
    assert_eq!(counters[210], Some(210), "main again at 3.5 s");
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn crossfade_from_below_maps_every_frame() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(4, 8_000));
    make_clip(
        &enc,
        &b,
        &ClipSpec {
            counter_base: B_BASE,
            ..ClipSpec::hd(4, 8_000)
        },
    );
    // Main 0–4 s; above it b's 0–1.5 s placed at 2.5 s, crossfading in from Main over
    // 500 ms (it starts at 2 s), then Main again from 3.5 s.
    let mut p = Project::new(4_000);
    p.advanced.tracks = vec![track(
        1,
        TrackKind::Video,
        vec![placed(1, library("b.mp4"), 0, 1_500, 2_500)],
    )];
    p.advanced.transitions = vec![Transition {
        track: 1,
        after_clip: klip_edit::advanced::FROM_BELOW,
        kind: TransitionKind::Crossfade,
        ms: 500,
    }];
    let comp = compose(&p, &source_info(&a));
    let media = vec![(library("b.mp4"), b.clone())];
    let out = dir.path().join("from-below.mp4");
    let encoders = encoders(&enc);
    run(&montage_request(
        Preset::Original,
        &a,
        &comp,
        &media,
        &out,
        &encoders,
    ));
    let counters = oracle::decode_counters(&out, 640, 360).unwrap();
    assert_eq!(counters.len() as u64, comp.frame_count(FPS));
    let (mut blended, mut bad) = (0, Vec::new());
    for (i, got) in counters.iter().enumerate() {
        match composed_counter(&comp, i as u64, FPS, 4 * FPS) {
            Some(want) if *got != Some(want) => bad.push((i, *got, want)),
            Some(_) => {}
            None => blended += 1,
        }
    }
    assert!(
        bad.is_empty(),
        "frames off the plan: {:?}",
        &bad[..bad.len().min(10)]
    );
    assert_eq!(blended, 30, "500 ms of crossfade at 60 fps");
    assert_eq!(counters[119], Some(119), "Main before the fade");
    assert_eq!(
        counters[150],
        Some(B_BASE + 30),
        "b from 2 s, full at 2.5 s"
    );
    assert_eq!(counters[210], Some(210), "Main again at 3.5 s");
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn dip_on_a_montage_track_goes_through_black() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(4, 8_000));
    make_clip(
        &enc,
        &b,
        &ClipSpec {
            counter_base: B_BASE,
            ..ClipSpec::hd(4, 8_000)
        },
    );
    // Main 0–4 s; above it b's 0–1.5 s at 1 s, then b's 2–3.5 s at 2.5 s with a 500 ms
    // dip between: black over 2–3 s, all black at 2.5 s, where Main used to show.
    let mut p = Project::new(4_000);
    p.advanced.tracks = vec![track(
        1,
        TrackKind::Video,
        vec![
            placed(1, library("b.mp4"), 0, 1_500, 1_000),
            placed(2, library("b.mp4"), 2_000, 3_500, 2_500),
        ],
    )];
    p.advanced.transitions = vec![Transition {
        track: 1,
        after_clip: 1,
        kind: TransitionKind::Dip,
        ms: 500,
    }];
    let comp = compose(&p, &source_info(&a));
    let media = vec![(library("b.mp4"), b.clone())];
    let out = dir.path().join("dip.mp4");
    let encoders = encoders(&enc);
    run(&montage_request(
        Preset::Original,
        &a,
        &comp,
        &media,
        &out,
        &encoders,
    ));
    let counters = oracle::decode_counters(&out, 640, 360).unwrap();
    assert_eq!(counters.len() as u64, comp.frame_count(FPS));
    let (mut dipped, mut bad) = (0, Vec::new());
    for (i, got) in counters.iter().enumerate() {
        match composed_counter(&comp, i as u64, FPS, 4 * FPS) {
            Some(want) if *got != Some(want) => bad.push((i, *got, want)),
            Some(_) => {}
            None => dipped += 1,
        }
    }
    assert!(
        bad.is_empty(),
        "frames off the plan: {:?}",
        &bad[..bad.len().min(10)]
    );
    // The plan around it holds (checked above): b up to 2 s, b again from 3 s.
    assert_eq!(dipped, 60, "1 s of dip at 60 fps");
    let mean = |s: f64| {
        let px = oracle::decode_gray_frame(&out, s, 160, 90).unwrap();
        px.iter().map(|&v| f64::from(v)).sum::<f64>() / px.len() as f64
    };
    let (before, half, cut) = (mean(1.9), mean(2.25), mean(2.5));
    println!("dip: mean luma {before:.1} before, {half:.1} half way, {cut:.1} at the cut");
    assert!(cut < 24.0, "black at the cut, not Main: {cut:.1}");
    assert!(
        half > cut + 8.0 && half < before * 0.8,
        "half way between: {half:.1} (before {before:.1})"
    );
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn zoom_shows_the_centre_and_colour_grades() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(3, 20_000));
    let zoomed = |zoom: f32| {
        let mut p = Project::new(3_000);
        let mut c = placed(1, Media::Main, 0, 3_000, 0);
        c.transform = vec![TransformKey {
            at_ms: 0,
            value: klip_edit::Transform {
                zoom,
                x: 0.0,
                y: 0.0,
            },
            ease: Ease::Hold,
        }];
        p.advanced.tracks = vec![track(1, TrackKind::Video, vec![c])];
        compose(&p, &source_info(&a))
    };
    let encoders = encoders(&enc);
    let comp = zoomed(2.0);
    let out = dir.path().join("zoom.mp4");
    run(&request(Preset::Original, &a, &comp, &out, &encoders));
    let (w, h) = (1280usize, 720usize);
    // Output frame 61 (1.0167 s) shows source frame 61 at 2x about the centre.
    let at = 61.0 / 60.0 + 0.001;
    let got = oracle::decode_gray_frame(&out, at, w, h).unwrap();
    let src = oracle::decode_gray_frame(&a, at, w, h).unwrap();
    // Reference: the middle half of the source, doubled (bilinear).
    let mut want = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let sx = (w / 4) as f64 + (x as f64 + 0.5) / 2.0 - 0.5;
            let sy = (h / 4) as f64 + (y as f64 + 0.5) / 2.0 - 0.5;
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
            let p = |xx: usize, yy: usize| f64::from(src[yy.min(h - 1) * w + xx.min(w - 1)]);
            let v = p(x0, y0) * (1.0 - fx) * (1.0 - fy)
                + p(x0 + 1, y0) * fx * (1.0 - fy)
                + p(x0, y0 + 1) * (1.0 - fx) * fy
                + p(x0 + 1, y0 + 1) * fx * fy;
            want[y * w + x] = v.round() as u8;
        }
    }
    let zoom_db = psnr(&got, &want);
    let plain_db = psnr(&got, &src);
    println!(
        "zoom 2x: {zoom_db:.1} dB against the scaled reference, {plain_db:.1} dB against the plain frame"
    );
    assert!(zoom_db >= 28.0, "{zoom_db:.1} dB");
    assert!(
        plain_db < zoom_db - 8.0,
        "the check tells zoomed from plain"
    );

    // Colour: +0.2 brightness lifts the picture by ~51 levels where nothing clips.
    let mut p = Project::new(3_000);
    p.advanced.colour.brightness = 0.2;
    let graded = compose(&p, &source_info(&a));
    let out2 = dir.path().join("graded.mp4");
    run(&request(Preset::Original, &a, &graded, &out2, &encoders));
    let g = oracle::decode_gray_frame(&out2, at, w, h).unwrap();
    // Where the source is mid-grey (nothing clips), the graded frame is ~51 brighter.
    let diffs: Vec<f64> = g
        .iter()
        .zip(&src)
        .filter(|(_, s)| (20..=180).contains(*s))
        .map(|(a, b)| f64::from(*a) - f64::from(*b))
        .collect();
    let lift = diffs.iter().sum::<f64>() / diffs.len() as f64;
    println!("brightness +0.2: mean luma +{lift:.1}");
    assert!((40.0..60.0).contains(&lift), "{lift:.1}");
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn music_ducks_under_a_voice() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(4, 8_000));
    let music = dir.path().join("music.wav");
    let voice = dir.path().join("voice.wav");
    write_wav(&music, 4.0, |t| {
        0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
    });
    write_wav(&voice, 1.0, |t| {
        0.3 * (2.0 * std::f64::consts::PI * 1_000.0 * t).sin()
    });
    let file = |p: &Path| Media::File {
        path: p.to_string_lossy().into_owned(),
    };
    let mut p = Project::new(4_000);
    // The clip's own sound off, so only music and voice remain.
    p.volumes = Volumes {
        game: 0.0,
        mic: 0.0,
    };
    p.advanced.tracks = vec![
        track(
            1,
            TrackKind::Music,
            vec![placed(1, file(&music), 0, 4_000, 0)],
        ),
        track(
            2,
            TrackKind::Audio,
            vec![placed(2, file(&voice), 0, 1_000, 2_000)],
        ),
    ];
    let comp = compose(&p, &source_info(&a));
    let media = vec![(file(&music), music.clone()), (file(&voice), voice.clone())];
    let out = dir.path().join("ducked.mp4");
    let encoders = encoders(&enc);
    run(&montage_request(
        Preset::Original,
        &a,
        &comp,
        &media,
        &out,
        &encoders,
    ));
    let s = oracle::decode_audio_mono(&out, 0, SAMPLE_RATE).unwrap();
    let before = tone_level(window(&s, 1.0, 1.5), SAMPLE_RATE, 440.0);
    let during = tone_level(window(&s, 2.4, 2.9), SAMPLE_RATE, 440.0);
    let voice_level = tone_level(window(&s, 2.4, 2.9), SAMPLE_RATE, 1_000.0);
    let db = 20.0 * (during / before).log10();
    println!(
        "music {before:.0} → {during:.0} under the voice ({db:.2} dB); voice {voice_level:.0}"
    );
    assert!((db + 12.0).abs() < 1.5, "ducked by {db:.2} dB, want −12");
    assert!(voice_level > before * 0.5, "the voice is in the mix");
    // Without ducking the music stays put.
    let mut flat = comp.clone();
    flat.ducking.enabled = false;
    let out2 = dir.path().join("flat.mp4");
    run(&montage_request(
        Preset::Original,
        &a,
        &flat,
        &media,
        &out2,
        &encoders,
    ));
    let s2 = oracle::decode_audio_mono(&out2, 0, SAMPLE_RATE).unwrap();
    let db2 = 20.0
        * (tone_level(window(&s2, 2.4, 2.9), SAMPLE_RATE, 440.0)
            / tone_level(window(&s2, 1.0, 1.5), SAMPLE_RATE, 440.0))
        .log10();
    assert!(db2.abs() < 0.5, "{db2:.2} dB without ducking");
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn custom_preset_honours_its_numbers() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(6, 20_000));
    let (_, comp) = plan(&Project::new(6_000), &source_info(&a));
    let out = dir.path().join("custom.mp4");
    let encoders = encoders(&enc);
    let custom = klip_export::CustomOutput {
        width: 640,
        height: 640,
        fps: 30,
        bitrate_kbps: 1_500,
        codec: klip_export::OutCodec::H264,
    };
    let summary = run(&request(Preset::Custom(custom), &a, &comp, &out, &encoders));
    let streams = oracle::probe_streams(&out).unwrap();
    let v = streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .unwrap();
    println!(
        "custom: {}x{} {:?} fps {:?}, {} bytes",
        v.width.unwrap_or(0),
        v.height.unwrap_or(0),
        v.avg_fps(),
        v.codec_name,
        summary.bytes
    );
    assert_eq!(
        (v.width, v.height),
        (Some(640), Some(360)),
        "fits the box, keeps the shape"
    );
    assert_eq!(v.codec_name.as_deref(), Some("h264"));
    assert!((v.avg_fps().unwrap() - 30.0).abs() < 0.1);
    assert_eq!(summary.frames, comp.frame_count(30));
    // Simple synthetic frames need less than the target, so the check is that the
    // bitrate reaches the encoder: never over it, and a lower one gives a smaller file.
    let kbps = summary.bytes as f64 * 8.0 / 6.0 / 1000.0;
    assert!(
        kbps < (1_500.0 + 160.0) * 1.3,
        "{kbps:.0} kbit/s for 1500 + 160"
    );
    let low = dir.path().join("custom-low.mp4");
    let starved = klip_export::CustomOutput {
        bitrate_kbps: 150,
        ..custom
    };
    let small = run(&request(
        Preset::Custom(starved),
        &a,
        &comp,
        &low,
        &encoders,
    ));
    let low_kbps = small.bytes as f64 * 8.0 / 6.0 / 1000.0;
    println!("custom: {kbps:.0} kbit/s at 1500, {low_kbps:.0} kbit/s at 150");
    assert!(low_kbps < kbps * 0.8, "{low_kbps:.0} vs {kbps:.0} kbit/s");
    assert!(
        low_kbps < (150.0 + 160.0) * 1.5,
        "{low_kbps:.0} kbit/s for 150 + 160"
    );
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn long_montage_stays_under_a_gigabyte() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(60, 8_000));
    make_clip(
        &enc,
        &b,
        &ClipSpec {
            counter_base: B_BASE,
            ..ClipSpec::hd(60, 8_000)
        },
    );
    let music = dir.path().join("music.wav");
    write_wav(&music, 60.0, |t| {
        0.2 * (2.0 * std::f64::consts::PI * 330.0 * t).sin()
    });
    let file = Media::File {
        path: music.to_string_lossy().into_owned(),
    };
    // Ten minutes: the main minute, then nine minutes alternating a and b with
    // crossfades, music all along, a zoom on every b.
    let mut p = Project::new(60_000);
    let mut clips = Vec::new();
    let mut transitions = Vec::new();
    for k in 0..9u32 {
        let key = if k % 2 == 0 { "b.mp4" } else { "a.mp4" };
        let mut c = placed(
            k + 1,
            library(key),
            0,
            60_000,
            60_000 + u64::from(k) * 60_000,
        );
        if key == "b.mp4" {
            c.transform = vec![
                TransformKey {
                    at_ms: 0,
                    value: klip_edit::Transform::default(),
                    ease: Ease::Linear,
                },
                TransformKey {
                    at_ms: 30_000,
                    value: klip_edit::Transform {
                        zoom: 1.5,
                        x: 0.1,
                        y: 0.0,
                    },
                    ease: Ease::Linear,
                },
            ];
        }
        clips.push(c);
        if k > 0 {
            transitions.push(Transition {
                track: 1,
                after_clip: k,
                kind: TransitionKind::Crossfade,
                ms: 1_000,
            });
        }
    }
    let music_clips = (0..10u32)
        .map(|k| placed(100 + k, file.clone(), 0, 60_000, u64::from(k) * 60_000))
        .collect();
    p.advanced.tracks = vec![
        track(1, TrackKind::Video, clips),
        track(2, TrackKind::Music, music_clips),
    ];
    p.advanced.transitions = transitions;
    let comp = compose(&p, &source_info(&a));
    println!(
        "montage: {} layers, {} ms",
        comp.layers.len(),
        comp.duration_ms
    );
    assert!(comp.duration_ms >= 590_000, "{}", comp.duration_ms);
    let media = vec![
        (library("a.mp4"), a.clone()),
        (library("b.mp4"), b.clone()),
        (file.clone(), music.clone()),
    ];
    let out = dir.path().join("long.mp4");
    let encoders = encoders(&enc);
    let pid = std::process::id();
    let base = klip_platform::process_usage(pid).unwrap().resident_bytes;
    let peak = Arc::new(std::sync::atomic::AtomicU64::new(base));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (peak, stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(u) = klip_platform::process_usage(pid) {
                    peak.fetch_max(u.resident_bytes, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    let t = Instant::now();
    let summary = run(&montage_request(
        Preset::Original,
        &a,
        &comp,
        &media,
        &out,
        &encoders,
    ));
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();
    let peak_mb = peak.load(Ordering::Relaxed) as f64 / 1e6;
    println!(
        "10-minute montage: {} frames in {:.0} s ({:.1}x realtime), {:.0} MB, peak working set {peak_mb:.0} MB (test process at start {:.0} MB)",
        summary.frames,
        t.elapsed().as_secs_f64(),
        comp.duration_ms as f64 / 1000.0 / t.elapsed().as_secs_f64(),
        summary.bytes as f64 / 1e6,
        base as f64 / 1e6
    );
    assert!(peak_mb < 1_000.0, "peak working set {peak_mb:.0} MB");
    let streams = oracle::probe_streams(&out).unwrap();
    let v = streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .unwrap();
    assert!((v.duration_secs().unwrap() - comp.duration_ms as f64 / 1000.0).abs() < 0.1);
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn sticker_sits_where_the_plan_puts_it() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(3, 20_000));
    // A 256x256 white square, opaque in its left half, half transparent in its right.
    let (sw, sh) = (256u32, 256u32);
    let mut rgba = Vec::new();
    for _y in 0..sh {
        for x in 0..sw {
            rgba.extend_from_slice(&[255, 255, 255, if x < sw / 2 { 255 } else { 128 }]);
        }
    }
    let bitmap = klip_edit::TextBitmap {
        frame_width: sw,
        frame_height: sh,
        x: 0,
        y: 0,
        width: sw,
        height: sh,
        rgba,
    };
    let sticker = dir.path().join("sticker.png");
    std::fs::write(&sticker, bitmap.png().unwrap()).unwrap();
    let image = Media::Image {
        path: sticker.to_string_lossy().into_owned(),
    };
    let mut p = Project::new(3_000);
    p.advanced.tracks = vec![track(
        1,
        TrackKind::Video,
        vec![placed(1, image.clone(), 0, 1_000, 1_000)],
    )];
    let comp = compose(&p, &source_info(&a));
    let media = vec![(image, sticker.clone())];
    let out = dir.path().join("sticker.mp4");
    let encoders = encoders(&enc);
    run(&montage_request(
        Preset::Original,
        &a,
        &comp,
        &media,
        &out,
        &encoders,
    ));
    let (w, h) = (1280usize, 720usize);
    // 256 px of a 1280-wide crop on a 1280-wide canvas: 256x256, centred at 512,232.
    let region = |g: &[u8], x0: usize, x1: usize| {
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in 242..478 {
            for x in x0..x1 {
                sum += f64::from(g[y * w + x]);
                n += 1.0;
            }
        }
        sum / n
    };
    let at = 1.5 + 0.5 / 60.0;
    let with = oracle::decode_gray_frame(&out, at, w, h).unwrap();
    let src = oracle::decode_gray_frame(&a, at, w, h).unwrap();
    let opaque = region(&with, 522, 630);
    let half = region(&with, 650, 758);
    let under = region(&src, 650, 758);
    let want_half = under + (255.0 - under) * 128.0 / 255.0;
    println!("sticker: opaque half {opaque:.1}, transparent half {half:.1} (want {want_half:.1})");
    assert!(opaque > 245.0, "{opaque:.1}");
    assert!(
        (half - want_half).abs() < 6.0,
        "{half:.1} vs {want_half:.1}"
    );
    // Before and after its time it is not there.
    for t in [0.5, 2.5] {
        let g = oracle::decode_gray_frame(&out, t + 0.5 / 60.0, w, h).unwrap();
        let s = oracle::decode_gray_frame(&a, t + 0.5 / 60.0, w, h).unwrap();
        assert!(psnr(&g, &s) > 35.0, "at {t} s the frame is the clip's");
    }
}

/// M9.4: `frame` draws what the export encodes. The montage of
/// `montage_crossfade_maps_every_frame` plus a muted music track whose file is gone
/// (muted sound is never read; this export failed before M9.4): frames before,
/// inside and after the crossfade carry the planned barcode and match the export's
/// frame at the same time.
#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn frames_match_the_export() {
    use klip_export::FrameRequest;
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    make_clip(&enc, &a, &ClipSpec::hd(4, 8_000));
    make_clip(
        &enc,
        &b,
        &ClipSpec {
            counter_base: B_BASE,
            ..ClipSpec::hd(4, 8_000)
        },
    );
    let mut p = Project::new(4_000);
    let gone = dir.path().join("gone.wav").to_string_lossy().into_owned();
    p.advanced.tracks = vec![
        track(
            1,
            TrackKind::Video,
            vec![
                placed(1, library("b.mp4"), 0, 1_500, 1_000),
                placed(2, library("b.mp4"), 2_000, 3_500, 2_500),
            ],
        ),
        Track {
            muted: true,
            hidden: false,
            ..track(
                2,
                TrackKind::Music,
                vec![placed(3, Media::File { path: gone }, 0, 4_000, 0)],
            )
        },
    ];
    p.advanced.transitions = vec![Transition {
        track: 1,
        after_clip: 1,
        kind: TransitionKind::Crossfade,
        ms: 500,
    }];
    let comp = compose(&p, &source_info(&a));
    let media = vec![(library("b.mp4"), b.clone())];
    let out = dir.path().join("montage.mp4");
    let encoders = encoders(&enc);
    run(&montage_request(
        Preset::Original,
        &a,
        &comp,
        &media,
        &out,
        &encoders,
    ));
    let (w, h) = (640, 360);
    for i in [0u64, 59, 60, 100, 125, 140, 150, 209, 210, 239] {
        let t = comp.frame_out_ms(i, FPS).unwrap();
        let started = Instant::now();
        let img = klip_export::frame::frame(&FrameRequest {
            source: &a,
            composition: &comp,
            media: &media,
            out_ms: t,
            width: 1280,
        })
        .unwrap();
        let took = started.elapsed();
        assert_eq!((img.width, img.height, img.out_ms), (1280, 720, t));
        let jpg = dir.path().join(format!("frame-{i}.jpg"));
        std::fs::write(&jpg, &img.jpeg).unwrap();
        let got = oracle::decode_gray_frame(&jpg, 0.0, w, h).unwrap();
        let exported = oracle::decode_gray_frame(&out, t as f64 / 1000.0, w, h).unwrap();
        let db = psnr(&got, &exported);
        let counter = barcode::read_gray(&got, w, h);
        println!(
            "frame {i} at {t} ms: counter {counter:?}, PSNR vs export {db:.1} dB, {} KB, {:.0} ms",
            img.jpeg.len() / 1024,
            took.as_secs_f64() * 1000.0
        );
        if let Some(want) = composed_counter(&comp, i, FPS, 4 * FPS) {
            assert_eq!(counter, Some(want), "frame {i}");
        }
        assert!(db >= 35.0, "frame {i}: {db:.1} dB");
    }
    // Past the end: the last frame.
    let last = klip_export::frame::frame(&FrameRequest {
        source: &a,
        composition: &comp,
        media: &media,
        out_ms: u64::MAX,
        width: 320,
    })
    .unwrap();
    assert_eq!(
        (last.width, last.height, last.out_ms),
        (320, 180, comp.duration_ms - 1)
    );
    // A width out of range is refused by name.
    let e = klip_export::frame::frame(&FrameRequest {
        source: &a,
        composition: &comp,
        media: &media,
        out_ms: 0,
        width: 8,
    })
    .err()
    .unwrap();
    assert!(e.to_string().contains("pixels wide"), "{e}");
}

#[test]
#[ignore = "hardware: a GPU encoder and ffmpeg"]
fn audio_split_keeps_each_track() {
    let Some(enc) = setup() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    make_clip(&enc, &clip, &ClipSpec::hd(4, 20_000));
    let out = dir.path().join("audio");
    let t0 = Instant::now();
    let files = klip_export::audio::split_audio(&clip, &out, 2, Duration::from_secs(30)).unwrap();
    println!("split 2 tracks of a 4 s clip in {:?}", t0.elapsed());
    assert_eq!(files.len(), 2);
    let game = onset_times(&files[0]);
    let mic = onset_times(&files[1]);
    println!("game {game:?}, mic {mic:?}");
    assert!(close(&game, &GAME_BEEPS, 0.005), "{game:?}");
    assert!(close(&mic, &MIC_BEEPS, 0.005), "{mic:?}");
    // A track the clip doesn't have: an error, and nothing left behind.
    let none = dir.path().join("none");
    assert!(klip_export::audio::split_audio(&clip, &none, 3, Duration::from_secs(30)).is_err());
    assert_eq!(
        std::fs::read_dir(&none).unwrap().count(),
        0,
        "no files left"
    );
    // Not a clip at all.
    let junk = dir.path().join("junk.mp4");
    std::fs::write(&junk, b"not an mp4").unwrap();
    let bad = dir.path().join("bad");
    assert!(klip_export::audio::split_audio(&junk, &bad, 1, Duration::from_secs(10)).is_err());
    assert_eq!(std::fs::read_dir(&bad).unwrap().count(), 0);
}
