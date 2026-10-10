//! Klip's export pipeline (ADR 0017): a `klip_edit::Render` plan plus a preset → an MP4
//! next to the clip. Decoding, cropping and scaling happen in one GStreamer pipeline
//! ([`source`]), text and volumes on the CPU ([`compose`]), encoding in another
//! pipeline with the recorder's hardware encoder chain ([`encode`]); Discord's size
//! limit is met by re-encoding at a lower bitrate ([`fit`]).
//!
//! Frame mapping: output frame `i` at `fps` shows the source frame with the largest
//! presentation time ≤ `Render::frame_source_ms(i)`: the same integer math as the
//! preview, so a cut lands on the same frame in both.
//!
//! Nothing here panics on a hostile plan or file: the plan is validated against the
//! probed file (size, length, an estimate of the output's bytes), every size is
//! bounded, pipeline errors are reported, and a cancelled or failed export leaves no
//! file behind (the MP4 is written atomically at the very end).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use gstreamer as gst;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::VideoFrameExt as _;
use klip_capture::CaptureError;
use klip_edit::{Render, TextBitmap, Volumes};
use klip_media::{Codec, NANOS_PER_MILLI};
use klip_mp4::{Marker, Mp4Options, Mp4Track};
use klip_probe::EncoderChoice;
use serde::{Deserialize, Serialize};

pub mod compose;
pub mod encode;
pub mod fit;
pub mod source;

use encode::{EncodeSettings, Encoded, Encoder};
use source::{SAMPLES_PER_MS, Source, SourceConfig};

/// Default Discord upload limit, bytes.
pub const DISCORD_LIMIT_BYTES: u64 = 10_000_000;
/// AAC bitrate of most presets, bit/s.
pub const AUDIO_BPS: u32 = 160_000;
/// AAC bitrate of the Discord preset, bit/s.
pub const DISCORD_AUDIO_BPS: u32 = 128_000;
/// Output frame rates are clamped to this.
pub const MAX_FPS: u32 = 240;
/// Bitrate bounds for the Original preset (kbit/s).
pub const ORIGINAL_KBPS: (u32, u32) = (2_000, 200_000);
/// Longest export, ms (the encoded output is held in memory until it is written).
pub const MAX_EXPORT_MS: u64 = 4 * 3_600_000;
/// Largest export, bytes, estimated from the bitrates before encoding (same reason).
pub const MAX_EXPORT_BYTES: u64 = 4 << 30;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("cancelled")]
    Cancelled,
    #[error("the edit cannot be exported: {0}")]
    Plan(String),
    #[error("the clip cannot be read: {0}")]
    Source(String),
    #[error("no working {} encoder", .0.name())]
    NoEncoder(Codec),
    #[error("encoding failed: {0}")]
    Encoder(String),
    #[error("GStreamer: {0}")]
    Gst(String),
    #[error("the file is still over the size limit after {0} passes")]
    TooLarge(u32),
    #[error("writing the file: {0}")]
    Mp4(#[from] klip_mp4::Mp4Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl From<CaptureError> for ExportError {
    fn from(e: CaptureError) -> Self {
        ExportError::Gst(e.to_string())
    }
}

/// The export presets (ADR 0017).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    /// Source size, rate, codec and bitrate.
    Original,
    /// Fits 1920x1080 (or 1080x1920 for a tall crop), ≤ 60 fps, 20 Mbit/s, the source codec.
    YouTube,
    /// Fits 1080x1920 (or 1920x1080 for a wide crop), ≤ 60 fps, 12 Mbit/s H.264.
    TikTok,
    /// H.264 + AAC 128 kbit/s under a byte limit (more passes when needed).
    Discord,
    /// The editor's preview proxy: H.264 fitting 1280x720 (or 720x1280), ≤ 60 fps,
    /// 8 Mbit/s. Not a user preset (`ALL` leaves it out).
    Proxy,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Original,
        Preset::YouTube,
        Preset::TikTok,
        Preset::Discord,
    ];

    /// The id used in commands and settings.
    pub fn id(self) -> &'static str {
        match self {
            Preset::Original => "original",
            Preset::YouTube => "youtube",
            Preset::TikTok => "tiktok",
            Preset::Discord => "discord",
            Preset::Proxy => "proxy",
        }
    }

    /// The name shown to the user and used in the file name (`<clip> - <name>.mp4`).
    pub fn name(self) -> &'static str {
        match self {
            Preset::Original => "Original",
            Preset::YouTube => "YouTube",
            Preset::TikTok => "TikTok",
            Preset::Discord => "Discord",
            Preset::Proxy => "Proxy",
        }
    }

    pub fn parse(id: &str) -> Option<Preset> {
        Preset::ALL
            .into_iter()
            .chain([Preset::Proxy])
            .find(|p| p.id() == id)
    }
}

/// Encoder choices per codec, best first (from `klip_probe::ranked`, or whatever the
/// caller has). A codec with no entry falls back to H.264.
#[derive(Clone, Debug, Default)]
pub struct Encoders {
    pub h264: Vec<EncoderChoice>,
    pub h265: Vec<EncoderChoice>,
    pub av1: Vec<EncoderChoice>,
}

impl Encoders {
    pub fn for_codec(&self, codec: Codec) -> &[EncoderChoice] {
        match codec {
            Codec::H264 => &self.h264,
            Codec::H265 => &self.h265,
            Codec::Av1 => &self.av1,
            Codec::Aac | Codec::Opus => &[],
        }
    }

    pub fn push(&mut self, choice: EncoderChoice) {
        match choice.codec {
            Codec::H264 => self.h264.push(choice),
            Codec::H265 => self.h265.push(choice),
            Codec::Av1 => self.av1.push(choice),
            Codec::Aac | Codec::Opus => {}
        }
    }

    /// Rebuild from element names (what crosses a process boundary).
    pub fn from_names(names: &[String]) -> Encoders {
        let mut out = Encoders::default();
        for codec in [Codec::H264, Codec::H265, Codec::Av1] {
            let all = klip_probe::candidates(codec, &[], None);
            for name in names {
                if let Some(c) = all.iter().find(|c| &c.element == name) {
                    out.push(c.clone());
                }
            }
        }
        out
    }

    pub fn names(&self) -> Vec<String> {
        self.h264
            .iter()
            .chain(&self.h265)
            .chain(&self.av1)
            .map(|c| c.element.clone())
            .collect()
    }
}

/// One export.
pub struct ExportRequest<'a> {
    pub source: &'a Path,
    pub render: &'a Render,
    pub preset: Preset,
    /// Where the MP4 goes; must not exist.
    pub out: &'a Path,
    pub encoders: &'a Encoders,
    /// Discord's byte limit.
    pub discord_limit_bytes: u64,
    /// Test knob: behave as if pass 1 overshot by this factor (0 = off), so the second
    /// pass can be exercised without a clip that really overshoots.
    pub pretend_overshoot: f64,
}

/// Where an export is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// 1-based encode pass.
    pub pass: u32,
    pub frames_done: u64,
    pub frames_total: u64,
}

/// What an export produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSummary {
    pub path: PathBuf,
    pub bytes: u64,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: String,
    pub encoder: String,
    pub bitrate_kbps: u32,
    pub passes: u32,
    pub frames: u64,
    pub elapsed_ms: u64,
}

/// What the probe found out about the source.
#[derive(Clone, Debug)]
struct Facts {
    width: u32,
    height: u32,
    fps: u32,
    codec: Option<Codec>,
    kbps: u32,
    /// Movie duration, if the file says.
    duration_ms: Option<u64>,
    audio_names: Vec<String>,
}

fn probe(path: &Path) -> Result<Facts, ExportError> {
    let mut f = std::fs::File::open(path)?;
    let bytes = f.metadata()?.len();
    let info = klip_mp4::probe(&mut f).map_err(|e| ExportError::Source(e.to_string()))?;
    let video = info
        .video
        .as_ref()
        .ok_or_else(|| ExportError::Source("the clip has no video track".into()))?;
    let fps = video
        .fps
        .filter(|f| f.is_finite() && *f >= 1.0)
        .map_or(60, |f| f.round() as u32)
        .clamp(1, MAX_FPS);
    let codec = match video.codec.as_str() {
        "H.264" => Some(Codec::H264),
        "HEVC" => Some(Codec::H265),
        "AV1" => Some(Codec::Av1),
        _ => None,
    };
    let kbps = info
        .duration_ms
        .filter(|d| *d > 0)
        .and_then(|d| bytes.saturating_mul(8).checked_div(d))
        .map_or(ORIGINAL_KBPS.0, |k| u32::try_from(k).unwrap_or(u32::MAX))
        .clamp(ORIGINAL_KBPS.0, ORIGINAL_KBPS.1);
    Ok(Facts {
        width: video.width,
        height: video.height,
        fps,
        codec,
        kbps,
        duration_ms: info.duration_ms.filter(|d| *d > 0),
        audio_names: info.audio_names.clone(),
    })
}

/// The settings of one pass, before the encoder is picked.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Output {
    width: u32,
    height: u32,
    fps: u32,
    codec: Codec,
    bitrate_kbps: u32,
    max_bitrate_kbps: Option<u32>,
    audio_bps: u32,
}

fn even(v: u32) -> u32 {
    (v & !1).max(2)
}

/// `w x h` scaled down (never up) to fit `max_w x max_h`, even sides.
fn fit_box(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (fw, fh) = klip_capture::fit_size(w.max(2), h.max(2), max_w.max(2), max_h.max(2));
    (even(fw), even(fh))
}

/// A `short`-pixel box in the crop's orientation (16:9 wide, 9:16 tall); the long side
/// is rounded up so a 16:9 picture is limited by its short side, not by a pixel.
fn box_for(cw: u32, ch: u32, short: u32) -> (u32, u32) {
    let long = short.saturating_mul(16).saturating_add(8) / 9;
    if cw >= ch {
        (long, short)
    } else {
        (short, long)
    }
}

fn output_for(
    preset: Preset,
    render: &Render,
    facts: &Facts,
    encoders: &Encoders,
    pass: Option<fit::FitPass>,
) -> Output {
    let (cw, ch) = (render.crop.width, render.crop.height);
    let src_codec = facts
        .codec
        .filter(|c| !encoders.for_codec(*c).is_empty())
        .unwrap_or(Codec::H264);
    match preset {
        Preset::Original => Output {
            width: even(cw),
            height: even(ch),
            fps: facts.fps,
            codec: src_codec,
            bitrate_kbps: facts.kbps,
            max_bitrate_kbps: None,
            audio_bps: AUDIO_BPS,
        },
        Preset::YouTube => {
            let (max_w, max_h) = box_for(cw, ch, 1080);
            let (w, h) = fit_box(cw, ch, max_w, max_h);
            Output {
                width: w,
                height: h,
                fps: facts.fps.min(60),
                codec: src_codec,
                bitrate_kbps: 20_000,
                max_bitrate_kbps: None,
                audio_bps: AUDIO_BPS,
            }
        }
        Preset::TikTok => {
            let (max_w, max_h) = box_for(cw, ch, 1080);
            let (w, h) = fit_box(cw, ch, max_w, max_h);
            Output {
                width: w,
                height: h,
                fps: facts.fps.min(60),
                codec: Codec::H264,
                bitrate_kbps: 12_000,
                max_bitrate_kbps: None,
                audio_bps: AUDIO_BPS,
            }
        }
        Preset::Proxy => {
            let (max_w, max_h) = box_for(cw, ch, 720);
            let (w, h) = fit_box(cw, ch, max_w, max_h);
            Output {
                width: w,
                height: h,
                fps: facts.fps.min(60),
                codec: Codec::H264,
                bitrate_kbps: 8_000,
                max_bitrate_kbps: None,
                audio_bps: AUDIO_BPS,
            }
        }
        Preset::Discord => {
            let p = pass.unwrap_or(fit::FitPass {
                bitrate_kbps: 4_000,
                max_height: 1080,
                fps_cap: 60,
            });
            let (max_w, max_h) = box_for(cw, ch, p.max_height);
            let (w, h) = fit_box(cw, ch, max_w, max_h);
            Output {
                width: w,
                height: h,
                fps: facts.fps.min(p.fps_cap),
                codec: Codec::H264,
                bitrate_kbps: p.bitrate_kbps,
                max_bitrate_kbps: Some(p.bitrate_kbps),
                audio_bps: DISCORD_AUDIO_BPS,
            }
        }
    }
}

/// The plan against the file: size, length, and a rough size of the result (the
/// encoded output lives in memory until it is written).
fn check_plan(render: &Render, facts: &Facts, out: &Output) -> Result<(), ExportError> {
    if facts.width != render.source.width || facts.height != render.source.height {
        return Err(ExportError::Plan(format!(
            "the plan is for a {}x{} clip, the file is {}x{}",
            render.source.width, render.source.height, facts.width, facts.height
        )));
    }
    if let Some(d) = facts.duration_ms
        && render.segments.iter().any(|s| s.src_end_ms > d)
    {
        return Err(ExportError::Plan(
            "the edit goes past the end of the clip".into(),
        ));
    }
    if render.output_ms > MAX_EXPORT_MS {
        return Err(ExportError::Plan(
            "the export is longer than 4 hours".into(),
        ));
    }
    let kbps = u64::from(out.bitrate_kbps).saturating_add(u64::from(out.audio_bps) / 1000);
    let estimate = kbps.saturating_mul(render.output_ms) / 8;
    if estimate > MAX_EXPORT_BYTES {
        return Err(ExportError::Plan(
            "the export would be larger than 4 GB".into(),
        ));
    }
    Ok(())
}

/// Export `req` to `req.out`, reporting progress and stopping when `cancel` is set.
pub fn export(
    req: &ExportRequest<'_>,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<ExportSummary, ExportError> {
    let started = Instant::now();
    req.render.validate().map_err(ExportError::Plan)?;
    if req.out.exists() {
        return Err(klip_mp4::Mp4Error::AlreadyExists(req.out.to_path_buf()).into());
    }
    gst::init().map_err(|e| ExportError::Gst(e.to_string()))?;
    let facts = probe(req.source)?;
    let mut pass_no: u32 = 1;
    let mut fit_pass: Option<fit::FitPass> = (req.preset == Preset::Discord).then(|| {
        fit::first_pass(
            req.discord_limit_bytes,
            req.render.output_ms,
            DISCORD_AUDIO_BPS / 1000,
        )
    });
    loop {
        let out = output_for(req.preset, req.render, &facts, req.encoders, fit_pass);
        check_plan(req.render, &facts, &out)?;
        let encoded = run_pass(req, &facts, &out, pass_no, progress, cancel)?;
        let (mp4, duration_ms) = mux(&encoded, req.render)?;
        let mut bytes = mp4.len() as u64;
        if req.preset == Preset::Discord && pass_no == 1 && req.pretend_overshoot > 1.0 {
            bytes = (bytes as f64 * req.pretend_overshoot) as u64;
        }
        let over = req.preset == Preset::Discord && bytes > req.discord_limit_bytes;
        if over {
            let prev = fit_pass.unwrap_or(fit::FitPass {
                bitrate_kbps: out.bitrate_kbps,
                max_height: out.height,
                fps_cap: out.fps,
            });
            let next = fit::next_pass(&prev, req.discord_limit_bytes, bytes);
            if pass_no >= fit::MAX_PASSES || next == prev {
                return Err(ExportError::TooLarge(pass_no));
            }
            fit_pass = Some(next);
            pass_no = pass_no.saturating_add(1);
            tracing::info!(
                bytes,
                limit = req.discord_limit_bytes,
                pass = pass_no,
                "export over the limit, another pass"
            );
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        write_atomic(req.out, &mp4)?;
        let frames = encoded.video.1.len() as u64;
        return Ok(ExportSummary {
            path: req.out.to_path_buf(),
            bytes: mp4.len() as u64,
            duration_ms,
            width: out.width,
            height: out.height,
            fps: out.fps,
            codec: out.codec.name().to_string(),
            encoder: encoded.encoder,
            bitrate_kbps: out.bitrate_kbps,
            passes: pass_no,
            frames,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
    }
}

/// `data` to `path` through a `.part` file in the same folder; never overwrites, and
/// never touches a `.part` it did not create (another export, or a killed one).
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), ExportError> {
    use std::io::Write as _;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(ExportError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "{} is in the way (another export, or one that was stopped)",
                    tmp.display()
                ),
            )));
        }
        Err(e) => return Err(e.into()),
    };
    let r = (|| -> std::io::Result<()> {
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        if path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "exists",
            ));
        }
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    match r {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(klip_mp4::Mp4Error::AlreadyExists(path.to_path_buf()).into())
        }
        other => Ok(other?),
    }
}

/// The MP4 bytes of a pass and its duration.
fn mux(encoded: &Encoded, render: &Render) -> Result<(Vec<u8>, u64), ExportError> {
    let (vinfo, vpackets) = &encoded.video;
    let mut tracks = vec![Mp4Track::new(vinfo, vpackets).named("Video")];
    if let Some((ainfo, apackets)) = &encoded.audio {
        tracks.push(Mp4Track::new(ainfo, apackets).named("Mix"));
    }
    let markers: Vec<Marker> = render
        .markers
        .iter()
        .map(|m| Marker {
            at: i64::try_from(m.out_ms.saturating_mul(NANOS_PER_MILLI as u64)).unwrap_or(i64::MAX),
            title: m.title.clone(),
        })
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut out = Vec::new();
    let summary = klip_mp4::write_mp4(
        &mut out,
        &tracks,
        &Mp4Options {
            creation_time_unix: now,
            force_co64: false,
            markers: &markers,
        },
    )?;
    let duration_ms = u64::try_from(summary.duration)
        .unwrap_or(0)
        .checked_div(NANOS_PER_MILLI as u64)
        .unwrap_or(0);
    Ok((out, duration_ms))
}

/// A text layer's bitmap, rendered once per pass.
struct Cue {
    id: u32,
    bitmap: TextBitmap,
}

fn start_encoder(out: &Output, encoders: &Encoders) -> Result<Encoder, ExportError> {
    let choices = encoders.for_codec(out.codec);
    if choices.is_empty() {
        return Err(ExportError::NoEncoder(out.codec));
    }
    let mut last = None;
    for choice in choices {
        let settings = EncodeSettings {
            encoder: choice.clone(),
            width: out.width,
            height: out.height,
            fps: out.fps,
            bitrate_kbps: out.bitrate_kbps,
            max_bitrate_kbps: out.max_bitrate_kbps,
            audio_bps: Some(out.audio_bps),
        };
        match encode::guarded(|| Encoder::start(&settings)) {
            Ok(e) => return Ok(e),
            Err(e) => {
                tracing::warn!(encoder = %choice.element, error = %e, "export encoder did not start");
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or(ExportError::NoEncoder(out.codec)))
}

/// A packed BGRA copy of `frame` with the active text cues blended in.
fn composed(
    frame: &source::Frame,
    out: &Output,
    cues: &[&Cue],
) -> Result<gst::Buffer, ExportError> {
    let vf = gst_video::VideoFrameRef::from_buffer_ref_readable(frame.buffer.as_ref(), &frame.info)
        .map_err(|_| ExportError::Source("a frame could not be read".into()))?;
    let stride = usize::try_from(vf.plane_stride().first().copied().unwrap_or(0)).unwrap_or(0);
    let row_bytes = (out.width as usize).saturating_mul(4);
    let data = vf
        .plane_data(0)
        .map_err(|_| ExportError::Source("a frame has no pixels".into()))?;
    let mut packed = Vec::with_capacity(row_bytes.saturating_mul(out.height as usize));
    for y in 0..out.height as usize {
        let start = y.saturating_mul(stride);
        let row = data
            .get(start..start.saturating_add(row_bytes))
            .ok_or_else(|| ExportError::Source("a frame is too short".into()))?;
        packed.extend_from_slice(row);
    }
    for cue in cues {
        compose::blend_rgba_over_bgra(&mut packed, out.width, out.height, row_bytes, &cue.bitmap);
    }
    Ok(gst::Buffer::from_mut_slice(packed))
}

/// The samples of one segment trimmed or padded to its output length.
fn fit_samples(mut samples: Vec<i16>, out_ms: u64) -> Vec<i16> {
    let want = usize::try_from(out_ms)
        .unwrap_or(usize::MAX)
        .saturating_mul(SAMPLES_PER_MS);
    samples.resize(want, 0);
    samples
}

/// Which slider a recorded track follows (`None`: not used in the mix).
fn volume_for(name: &str, has_separate: bool, v: &Volumes) -> Option<f32> {
    match name {
        "Game" | "System" => Some(v.game),
        "Microphone" | "Voice" => Some(v.mic),
        "Mix" => (!has_separate).then_some(v.game),
        _ => (!has_separate).then_some(v.game),
    }
}

fn run_pass(
    req: &ExportRequest<'_>,
    facts: &Facts,
    out: &Output,
    pass: u32,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Encoded, ExportError> {
    let render = req.render;
    let cues: Vec<Cue> = render
        .texts
        .iter()
        .filter_map(|t| {
            klip_edit::render_layer(t, out.width, out.height)
                .ok()
                .map(|bitmap| Cue { id: t.id, bitmap })
        })
        .collect();
    let source = Source::open(
        req.source,
        SourceConfig {
            src_width: facts.width,
            src_height: facts.height,
            crop: render.crop,
            out_width: out.width,
            out_height: out.height,
        },
    )?;
    let mut encoder = start_encoder(out, req.encoders)?;
    let frames_total = render.frame_count(out.fps);
    let mut report = |done: u64| {
        progress(Progress {
            pass,
            frames_done: done,
            frames_total,
        });
    };
    report(0);
    let mut audio: Vec<Vec<i16>> = vec![Vec::new(); source.audio_tracks()];
    let mut i: u64 = 0;
    // The last frame of the previous segment: what a segment that lies past the last
    // video frame (the movie is a few ms longer than the video) keeps showing.
    let mut last: Option<source::Frame> = None;
    for seg in &render.segments {
        source.seek(seg.src_start_ms, seg.src_end_ms, seg.speed.rate())?;
        let mut cur: Option<source::Frame> = None;
        let mut next = source.next_frame()?;
        while let Some(t) = render.frame_out_ms(i, out.fps) {
            if t >= seg.out_end_ms {
                break;
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(ExportError::Cancelled);
            }
            let src_ms = render
                .frame_source_ms(i, out.fps)
                .ok_or_else(|| ExportError::Plan("frame outside the plan".into()))?;
            while next.as_ref().is_some_and(|n| n.pts_ms <= src_ms) {
                cur = next.take();
                next = source.next_frame()?;
            }
            let frame = cur
                .as_ref()
                .or(next.as_ref())
                .or(last.as_ref())
                .ok_or_else(|| ExportError::Source("the clip has no frames in a segment".into()))?;
            let active: Vec<&Cue> = render
                .texts_at(t)
                .filter_map(|l| cues.iter().find(|c| c.id == l.id))
                .collect();
            let buffer = if active.is_empty()
                && frame.info.width() == out.width
                && frame.info.height() == out.height
            {
                frame.buffer.copy()
            } else {
                composed(frame, out, &active)?
            };
            encoder.push_frame(buffer, i)?;
            i = i.saturating_add(1);
            if i.is_multiple_of(15) {
                report(i);
            }
        }
        // Run the segment out so its audio is complete.
        while let Some(f) = source.next_frame()? {
            next = Some(f);
        }
        if let Some(f) = cur.take().or(next.take()) {
            last = Some(f);
        }
        let out_len = seg.out_end_ms.saturating_sub(seg.out_start_ms);
        for (track, samples) in audio.iter_mut().zip(source.drain_audio()?) {
            track.extend(fit_samples(samples, out_len));
        }
    }
    report(i);
    drop(last);
    drop(source);
    // The mix: Game + Microphone (or Mix alone) with the editor's sliders.
    let names: Vec<&str> = (0..audio.len())
        .map(|i| facts.audio_names.get(i).map_or("", String::as_str))
        .collect();
    let has_separate = names
        .iter()
        .any(|n| matches!(*n, "Game" | "System" | "Microphone" | "Voice"));
    let inputs: Vec<compose::MixInput<'_>> = audio
        .iter()
        .zip(&names)
        .filter_map(|(samples, name)| {
            volume_for(name, has_separate, &render.volumes)
                .map(|volume| compose::MixInput { samples, volume })
        })
        .collect();
    let mixed = compose::mix(&inputs);
    let total = fit_samples(mixed, render.output_ms);
    encoder.push_audio(&total)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klip_edit::{Project, SourceInfo};

    fn facts() -> Facts {
        Facts {
            width: 2560,
            height: 1440,
            fps: 120,
            codec: Some(Codec::H265),
            kbps: 40_000,
            duration_ms: Some(10_000),
            audio_names: vec!["Mix".into(), "Game".into(), "Microphone".into()],
        }
    }

    fn source() -> SourceInfo {
        SourceInfo {
            width: 2560,
            height: 1440,
            duration_ms: 10_000,
            fps: Some(120.0),
            bookmarks_ms: vec![],
        }
    }

    fn render() -> Render {
        Render::new(&Project::new(10_000), &source())
    }

    #[test]
    fn presets_pick_sizes_rates_and_codecs() {
        let enc = Encoders::default();
        let o = output_for(Preset::Original, &render(), &facts(), &enc, None);
        assert_eq!(
            (o.width, o.height, o.fps, o.codec, o.bitrate_kbps),
            (2560, 1440, 120, Codec::H264, 40_000)
        );
        let o = output_for(Preset::YouTube, &render(), &facts(), &enc, None);
        assert_eq!(
            (o.width, o.height, o.fps, o.bitrate_kbps),
            (1920, 1080, 60, 20_000)
        );
        let o = output_for(Preset::TikTok, &render(), &facts(), &enc, None);
        assert_eq!((o.width, o.height, o.codec), (1920, 1080, Codec::H264));
        let p = fit::FitPass {
            bitrate_kbps: 1_000,
            max_height: 720,
            fps_cap: 30,
        };
        let o = output_for(Preset::Discord, &render(), &facts(), &enc, Some(p));
        assert_eq!(
            (o.width, o.height, o.fps, o.bitrate_kbps, o.max_bitrate_kbps),
            (1280, 720, 30, 1_000, Some(1_000))
        );
        assert_eq!(o.audio_bps, DISCORD_AUDIO_BPS);
    }

    #[test]
    fn tall_crops_fit_their_boxes() {
        let mut p = Project::new(10_000);
        p.format.kind = klip_edit::FormatKind::Tall;
        let r = Render::new(&p, &source());
        // A 810x1440 crop fits every 1080-short-side box as it is: never scaled up.
        for preset in [Preset::YouTube, Preset::TikTok] {
            let o = output_for(preset, &r, &facts(), &Encoders::default(), None);
            assert_eq!((o.width, o.height), (810, 1440), "{preset:?}");
        }
        let o = output_for(
            Preset::Discord,
            &r,
            &facts(),
            &Encoders::default(),
            Some(fit::FitPass {
                bitrate_kbps: 1_000,
                max_height: 720,
                fps_cap: 30,
            }),
        );
        assert_eq!((o.width, o.height), (720, 1280));
    }

    #[test]
    fn plans_are_checked_against_the_file() {
        let o = output_for(
            Preset::YouTube,
            &render(),
            &facts(),
            &Encoders::default(),
            None,
        );
        check_plan(&render(), &facts(), &o).unwrap();
        let mut short = facts();
        short.duration_ms = Some(9_000);
        assert!(check_plan(&render(), &short, &o).is_err());
        let mut wide = facts();
        wide.width = 1920;
        assert!(check_plan(&render(), &wide, &o).is_err());
        let mut long = render();
        long.output_ms = MAX_EXPORT_MS + 1;
        assert!(check_plan(&long, &facts(), &o).is_err());
        let mut huge = o.clone();
        huge.bitrate_kbps = 200_000;
        let mut r = render();
        r.output_ms = 3_600_000;
        assert!(check_plan(&r, &facts(), &huge).is_err());
    }

    #[test]
    fn preset_ids_round_trip() {
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.id()), Some(p));
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Preset>(&json).unwrap(), p);
        }
        assert_eq!(Preset::parse("Discord"), None);
    }

    #[test]
    fn volumes_follow_track_names() {
        let v = Volumes {
            game: 0.5,
            mic: 2.0,
        };
        assert_eq!(volume_for("Game", true, &v), Some(0.5));
        assert_eq!(volume_for("Microphone", true, &v), Some(2.0));
        assert_eq!(volume_for("Mix", true, &v), None);
        assert_eq!(volume_for("Mix", false, &v), Some(0.5));
        assert_eq!(volume_for("", false, &v), Some(0.5));
    }

    #[test]
    fn samples_fit_their_segment() {
        assert_eq!(fit_samples(vec![1; 10], 1).len(), SAMPLES_PER_MS);
        assert_eq!(fit_samples(vec![], 3).len(), 3 * SAMPLES_PER_MS);
    }

    #[test]
    fn a_stray_part_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("x.mp4");
        let part = dir.path().join("x.mp4.part");
        std::fs::write(&part, b"someone else's").unwrap();
        let e = write_atomic(&out, b"data").unwrap_err();
        assert!(matches!(e, ExportError::Io(_)), "{e}");
        assert_eq!(std::fs::read(&part).unwrap(), b"someone else's");
        std::fs::remove_file(&part).unwrap();
        write_atomic(&out, b"data").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"data");
        assert!(!part.exists());
        assert!(matches!(
            write_atomic(&out, b"again").unwrap_err(),
            ExportError::Mp4(klip_mp4::Mp4Error::AlreadyExists(_))
        ));
        assert!(!part.exists());
    }
}
