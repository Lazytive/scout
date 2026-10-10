//! Klip's export pipeline (ADR 0017, ADR 0018): a `klip_edit::Composition` (a project in
//! either mode) plus a preset → an MP4 next to the clip. [`render`] plays every layer
//! from its own decoding pipeline ([`source`]), composes each output frame in this
//! process ([`canvas`]), mixes the sound with volume keyframes and ducking ([`mixer`]),
//! and feeds another pipeline with the recorder's hardware encoder chain ([`encode`]);
//! Discord's size limit is met by re-encoding at a lower bitrate ([`fit`]).
//!
//! Frame mapping: output frame `i` at `fps` shows, per layer, the source frame with the
//! largest presentation time ≤ `Layer::source_ms(i·1000/fps)`: the same integer math
//! as the preview, so a cut lands on the same frame in both.
//!
//! Nothing here panics on a hostile plan or file: the plan is validated against the
//! probed file (size, length, an estimate of the output's bytes), every size is
//! bounded, pipeline errors are reported, and a cancelled or failed export leaves no
//! file behind (packets are spooled next to the output, the MP4 is written to a
//! `.part` and renamed at the very end; see [`files`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use gstreamer as gst;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::VideoFrameExt as _;
use klip_capture::CaptureError;
use klip_edit::advanced::Source as Media;
use klip_edit::compose::LayerKind;
use klip_edit::{Composition, TextBitmap};
use klip_media::{Codec, NANOS_PER_MILLI};
use klip_mp4::{Marker, Mp4Options};
use klip_probe::EncoderChoice;
use serde::{Deserialize, Serialize};

pub mod canvas;
pub mod compose;
pub mod encode;
pub mod files;
pub mod fit;
pub mod mixer;
pub mod render;
pub mod source;

use encode::{EncodeSettings, Encoded, Encoder};
use files::OwnedFile;
use render::MediaFile;

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
/// Bitrate bounds for a Custom export (kbit/s).
pub const CUSTOM_KBPS: (u32, u32) = (100, 200_000);
/// Longest side of an export.
pub const MAX_SIDE: u32 = 4096;
/// Most files one export reads.
pub const MAX_MEDIA: usize = 64;
/// Longest export, ms.
pub const MAX_EXPORT_MS: u64 = 4 * 3_600_000;
/// Largest export, bytes, estimated from the bitrates before encoding (a sanity bound:
/// the packets are spooled to disk, not held in memory).
pub const MAX_EXPORT_BYTES: u64 = 64 << 30;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("cancelled")]
    Cancelled,
    #[error("the edit cannot be exported: {0}")]
    Plan(String),
    #[error("the clip cannot be read: {0}")]
    Source(String),
    /// A file of the edit is gone (named: "the clip X", "the picture Y").
    #[error("{0} is missing")]
    Missing(String),
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
    /// The export dialog's own size, rate, bitrate and codec.
    Custom(CustomOutput),
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
            Preset::Custom(_) => "custom",
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
            Preset::Custom(_) => "Custom",
        }
    }

    pub fn parse(id: &str) -> Option<Preset> {
        Preset::ALL
            .into_iter()
            .chain([Preset::Proxy])
            .find(|p| p.id() == id)
    }
}

/// The numbers of a Custom export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomOutput {
    /// The box the picture is fitted into, up or down (its shape stays the edit's
    /// format), 2…[`MAX_SIDE`].
    pub width: u32,
    pub height: u32,
    /// 1…[`MAX_FPS`].
    pub fps: u32,
    /// Clamped to [`CUSTOM_KBPS`].
    pub bitrate_kbps: u32,
    /// Falls back to H.264 when no encoder for it is available.
    pub codec: OutCodec,
}

/// Video codecs an export can be asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutCodec {
    H264,
    Hevc,
    Av1,
}

impl OutCodec {
    pub fn codec(self) -> Codec {
        match self {
            OutCodec::H264 => Codec::H264,
            OutCodec::Hevc => Codec::H265,
            OutCodec::Av1 => Codec::Av1,
        }
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
    /// The main clip (`Source::Main` in the composition).
    pub source: &'a Path,
    /// What to render: `klip_edit::compose` of the project over `source`.
    pub composition: &'a Composition,
    /// Where the composition's other sources are (`Library`, `File`, `Image`), as the
    /// engine resolved them; a source without an entry, or whose file is gone, fails
    /// the export by name (never silently dropped).
    pub media: &'a [(Media, PathBuf)],
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
    crop: klip_edit::Rect,
    facts: &Facts,
    encoders: &Encoders,
    pass: Option<fit::FitPass>,
) -> Output {
    let (cw, ch) = (crop.width, crop.height);
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
        Preset::Custom(c) => {
            let (bw, bh) = (c.width.clamp(2, MAX_SIDE), c.height.clamp(2, MAX_SIDE));
            let (w, h) = klip_edit::compose::fit_contain(cw, ch, bw, bh);
            let codec = c.codec.codec();
            Output {
                width: w.min(MAX_SIDE),
                height: h.min(MAX_SIDE),
                fps: c.fps.clamp(1, MAX_FPS),
                codec: if encoders.for_codec(codec).is_empty() {
                    Codec::H264
                } else {
                    codec
                },
                bitrate_kbps: c.bitrate_kbps.clamp(CUSTOM_KBPS.0, CUSTOM_KBPS.1),
                max_bitrate_kbps: None,
                audio_bps: AUDIO_BPS,
            }
        }
    }
}

/// How many layers play at once, at most (muted sound layers never play).
fn most_running(comp: &Composition) -> usize {
    let mut events: Vec<(u64, i32)> = Vec::new();
    for l in comp.layers.iter().filter(|l| {
        !(l.muted && matches!(l.kind, LayerKind::Audio | LayerKind::Music)) && l.end_ms > l.at_ms
    }) {
        events.push((l.at_ms, 1));
        events.push((l.end_ms, -1));
    }
    events.sort_unstable();
    let (mut now, mut most) = (0i32, 0i32);
    for (_, d) in events {
        now = now.saturating_add(d);
        most = most.max(now);
    }
    usize::try_from(most).unwrap_or(0)
}

/// The composition against the main clip and the output: size, crop, length, caps on
/// everything a hostile composition could inflate, and a rough size of the result.
fn check_plan(comp: &Composition, main: &Facts, out: &Output) -> Result<(), ExportError> {
    let plan = |m: &str| Err(ExportError::Plan(m.to_string()));
    if comp.duration_ms == 0 {
        return plan("the edit is empty");
    }
    if main.width != comp.source.width || main.height != comp.source.height {
        return Err(ExportError::Plan(format!(
            "the plan is for a {}x{} clip, the file is {}x{}",
            comp.source.width, comp.source.height, main.width, main.height
        )));
    }
    let c = comp.crop;
    if c.width < 2
        || c.height < 2
        || c.x.saturating_add(c.width) > main.width
        || c.y.saturating_add(c.height) > main.height
    {
        return plan("the format's crop is outside the clip");
    }
    if let Some(d) = main.duration_ms
        && comp
            .layers
            .iter()
            .any(|l| l.source == Media::Main && l.src_out_ms > d)
    {
        return plan("the edit goes past the end of the clip");
    }
    if comp.duration_ms > MAX_EXPORT_MS {
        return plan("the export is longer than 4 hours");
    }
    if comp.layers.len() > klip_edit::compose::MAX_LAYERS
        || comp.texts.len() > klip_edit::MAX_TEXT_LAYERS
        || comp.markers.len() > klip_edit::advanced::MAX_MARKERS
    {
        return plan("the edit has too many layers");
    }
    if comp.layers.iter().any(|l| {
        l.transform.len() > klip_edit::advanced::MAX_KEYFRAMES
            || l.opacity.len() > klip_edit::advanced::MAX_KEYFRAMES
            || l.volume.len() > klip_edit::advanced::MAX_KEYFRAMES
            || l.speed.len() > klip_edit::advanced::MAX_SPEED_POINTS
    }) {
        return plan("a layer has too many keyframes");
    }
    if most_running(comp) > render::MAX_RUNNING {
        return Err(ExportError::Plan(format!(
            "more than {} clips play at the same time",
            render::MAX_RUNNING
        )));
    }
    let kbps = u64::from(out.bitrate_kbps).saturating_add(u64::from(out.audio_bps) / 1000);
    let estimate = kbps.saturating_mul(comp.duration_ms) / 8;
    if estimate > MAX_EXPORT_BYTES {
        return plan("the export would be larger than 64 GB");
    }
    Ok(())
}

/// How a source is named in an error (no folders: they may hold personal names).
fn describe(source: &Media) -> String {
    let file = |p: &str| {
        Path::new(p)
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    match source {
        Media::Main => "the clip".into(),
        Media::Library { key } => format!("the clip {}", file(key)),
        Media::File { path } => format!("the file {}", file(path)),
        Media::Image { path } => format!("the picture {}", file(path)),
    }
}

/// Every file the composition plays, probed once; a missing one fails by name.
fn media_files(req: &ExportRequest<'_>, main: &Facts) -> Result<Vec<MediaFile>, ExportError> {
    let mut files = vec![MediaFile {
        source: Media::Main,
        path: req.source.to_path_buf(),
        facts: Some(main.clone()),
    }];
    for l in &req.composition.layers {
        if l.muted && matches!(l.kind, LayerKind::Audio | LayerKind::Music) {
            continue;
        }
        if files.iter().any(|m| m.source == l.source) {
            continue;
        }
        if files.len() >= MAX_MEDIA {
            return Err(ExportError::Plan("the edit uses too many files".into()));
        }
        let missing = || ExportError::Missing(describe(&l.source));
        let path = req
            .media
            .iter()
            .find(|(s, _)| *s == l.source)
            .map(|(_, p)| p.clone())
            .ok_or_else(missing)?;
        if !path.is_file() {
            return Err(missing());
        }
        let facts = match l.kind {
            LayerKind::Video => Some(
                probe(&path)
                    .map_err(|e| ExportError::Source(format!("{}: {e}", describe(&l.source))))?,
            ),
            _ => probe(&path).ok(),
        };
        files.push(MediaFile {
            source: l.source.clone(),
            path,
            facts,
        });
    }
    Ok(files)
}

/// Export `req` to `req.out`, reporting progress and stopping when `cancel` is set.
pub fn export(
    req: &ExportRequest<'_>,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<ExportSummary, ExportError> {
    let started = Instant::now();
    if req.out.exists() {
        return Err(klip_mp4::Mp4Error::AlreadyExists(req.out.to_path_buf()).into());
    }
    let comp = req.composition;
    // What can be refused without reading anything.
    if comp.duration_ms == 0 || comp.duration_ms > MAX_EXPORT_MS {
        return Err(ExportError::Plan("the edit is empty or too long".into()));
    }
    let c = comp.crop;
    if c.x.saturating_add(c.width) > comp.source.width
        || c.y.saturating_add(c.height) > comp.source.height
    {
        return Err(ExportError::Plan(
            "the format's crop is outside the clip".into(),
        ));
    }
    gst::init().map_err(|e| ExportError::Gst(e.to_string()))?;
    let facts = probe(req.source)?;
    let media = media_files(req, &facts)?;
    let mut pass_no: u32 = 1;
    let mut fit_pass: Option<fit::FitPass> = (req.preset == Preset::Discord).then(|| {
        fit::first_pass(
            req.discord_limit_bytes,
            comp.duration_ms,
            DISCORD_AUDIO_BPS / 1000,
        )
    });
    loop {
        let out = output_for(req.preset, comp.crop, &facts, req.encoders, fit_pass);
        check_plan(comp, &facts, &out)?;
        let spool = files::with_suffix(req.out, ".spool");
        let encoded = render::run_pass(req, &media, &out, &spool, pass_no, progress, cancel)?;
        let (part, real_bytes, duration_ms) = mux(&encoded, &comp.markers, req.out)?;
        let frames = encoded.video.1.len() as u64;
        let encoder = encoded.encoder.clone();
        drop(encoded);
        let mut bytes = real_bytes;
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
        part.commit(req.out)?;
        return Ok(ExportSummary {
            path: req.out.to_path_buf(),
            bytes: real_bytes,
            duration_ms,
            width: out.width,
            height: out.height,
            fps: out.fps,
            codec: out.codec.name().to_string(),
            encoder,
            bitrate_kbps: out.bitrate_kbps,
            passes: pass_no,
            frames,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
    }
}

/// Mux a pass into `<out>.part` (removed again when the result is dropped uncommitted):
/// the part, its size and its duration.
fn mux(
    encoded: &Encoded,
    markers: &[klip_edit::advanced::Marker],
    out: &Path,
) -> Result<(OwnedFile, u64, u64), ExportError> {
    use std::io::{BufReader, BufWriter, Write as _};
    let (vinfo, vsamples) = &encoded.video;
    let mut tracks = vec![klip_mp4::SpooledTrack {
        info: vinfo,
        samples: vsamples,
        name: Some("Video"),
        enabled: true,
    }];
    if let Some((ainfo, asamples)) = &encoded.audio {
        tracks.push(klip_mp4::SpooledTrack {
            info: ainfo,
            samples: asamples,
            name: Some("Mix"),
            enabled: true,
        });
    }
    let markers: Vec<Marker> = markers
        .iter()
        .map(|m| Marker {
            at: i64::try_from(m.at_ms.saturating_mul(NANOS_PER_MILLI as u64)).unwrap_or(i64::MAX),
            title: m.title.clone(),
        })
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut payload = BufReader::with_capacity(1 << 20, std::fs::File::open(encoded.spool.path())?);
    let (part, file) = OwnedFile::create(&files::with_suffix(out, ".part"))?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let summary = klip_mp4::write_mp4_spooled(
        &mut w,
        &tracks,
        &Mp4Options {
            creation_time_unix: now,
            force_co64: false,
            markers: &markers,
        },
        &mut payload,
    )?;
    w.flush()?;
    let file = w
        .into_inner()
        .map_err(|e| ExportError::Io(e.into_error()))?;
    file.sync_all()?;
    let bytes = file.metadata()?.len();
    drop(file);
    let duration_ms = u64::try_from(summary.duration)
        .unwrap_or(0)
        .checked_div(NANOS_PER_MILLI as u64)
        .unwrap_or(0);
    Ok((part, bytes, duration_ms))
}

/// A text layer's bitmap, rendered once per pass.
struct Cue {
    id: u32,
    bitmap: TextBitmap,
}

fn start_encoder(out: &Output, encoders: &Encoders, spool: &Path) -> Result<Encoder, ExportError> {
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
        match encode::guarded(|| Encoder::start(&settings, spool)) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use klip_edit::{Project, SourceInfo, compose};

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

    fn comp() -> Composition {
        compose(&Project::new(10_000), &source())
    }

    fn crop() -> klip_edit::Rect {
        comp().crop
    }

    #[test]
    fn presets_pick_sizes_rates_and_codecs() {
        let enc = Encoders::default();
        let o = output_for(Preset::Original, crop(), &facts(), &enc, None);
        assert_eq!(
            (o.width, o.height, o.fps, o.codec, o.bitrate_kbps),
            (2560, 1440, 120, Codec::H264, 40_000)
        );
        let o = output_for(Preset::YouTube, crop(), &facts(), &enc, None);
        assert_eq!(
            (o.width, o.height, o.fps, o.bitrate_kbps),
            (1920, 1080, 60, 20_000)
        );
        let o = output_for(Preset::TikTok, crop(), &facts(), &enc, None);
        assert_eq!((o.width, o.height, o.codec), (1920, 1080, Codec::H264));
        let p = fit::FitPass {
            bitrate_kbps: 1_000,
            max_height: 720,
            fps_cap: 30,
        };
        let o = output_for(Preset::Discord, crop(), &facts(), &enc, Some(p));
        assert_eq!(
            (o.width, o.height, o.fps, o.bitrate_kbps, o.max_bitrate_kbps),
            (1280, 720, 30, 1_000, Some(1_000))
        );
        assert_eq!(o.audio_bps, DISCORD_AUDIO_BPS);
    }

    #[test]
    fn custom_honours_its_numbers() {
        let enc = Encoders::default();
        let c = |width, height, fps, bitrate_kbps, codec| {
            Preset::Custom(CustomOutput {
                width,
                height,
                fps,
                bitrate_kbps,
                codec,
            })
        };
        let o = output_for(
            c(1280, 720, 30, 6_000, OutCodec::H264),
            crop(),
            &facts(),
            &enc,
            None,
        );
        assert_eq!(
            (o.width, o.height, o.fps, o.bitrate_kbps, o.codec),
            (1280, 720, 30, 6_000, Codec::H264)
        );
        // Up to 4K from a 1440p clip; the shape stays the format's.
        let o = output_for(
            c(3840, 3840, 60, 50_000, OutCodec::Hevc),
            crop(),
            &facts(),
            &enc,
            None,
        );
        assert_eq!((o.width, o.height), (3840, 2160));
        assert_eq!(o.codec, Codec::H264, "no HEVC encoder in this list: H.264");
        let o = output_for(
            c(0, 99_999, 0, 0, OutCodec::Av1),
            crop(),
            &facts(),
            &enc,
            None,
        );
        assert_eq!((o.width, o.height), (2, 2));
        assert_eq!((o.fps, o.bitrate_kbps), (1, CUSTOM_KBPS.0));
        let o = output_for(
            c(4096, 4096, 999, u32::MAX, OutCodec::H264),
            crop(),
            &facts(),
            &enc,
            None,
        );
        assert_eq!(
            (o.width, o.fps, o.bitrate_kbps),
            (4096, MAX_FPS, CUSTOM_KBPS.1)
        );
        assert_eq!(Preset::parse("custom"), None, "custom needs its numbers");
        assert_eq!(c(2, 2, 1, 1, OutCodec::H264).name(), "Custom");
    }

    #[test]
    fn tall_crops_fit_their_boxes() {
        let mut p = Project::new(10_000);
        p.format.kind = klip_edit::FormatKind::Tall;
        let tall = compose(&p, &source()).crop;
        // A 810x1440 crop fits every 1080-short-side box as it is: never scaled up.
        for preset in [Preset::YouTube, Preset::TikTok] {
            let o = output_for(preset, tall, &facts(), &Encoders::default(), None);
            assert_eq!((o.width, o.height), (810, 1440), "{preset:?}");
        }
        let o = output_for(
            Preset::Discord,
            tall,
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
            crop(),
            &facts(),
            &Encoders::default(),
            None,
        );
        check_plan(&comp(), &facts(), &o).unwrap();
        let mut short = facts();
        short.duration_ms = Some(9_000);
        assert!(check_plan(&comp(), &short, &o).is_err());
        let mut wide = facts();
        wide.width = 1920;
        assert!(check_plan(&comp(), &wide, &o).is_err());
        let mut long = comp();
        long.duration_ms = MAX_EXPORT_MS + 1;
        assert!(check_plan(&long, &facts(), &o).is_err());
        let mut huge = o.clone();
        huge.bitrate_kbps = 200_000;
        let mut c = comp();
        c.duration_ms = 3_600_000;
        assert!(check_plan(&c, &facts(), &huge).is_err());
        let mut empty = comp();
        empty.duration_ms = 0;
        assert!(check_plan(&empty, &facts(), &o).is_err());
        let mut outside = comp();
        outside.crop.x = 1;
        assert!(check_plan(&outside, &facts(), &o).is_err());
        let mut keys = comp();
        if let Some(l) = keys.layers.first_mut() {
            l.volume = vec![klip_edit::Keyframe::default(); klip_edit::advanced::MAX_KEYFRAMES + 1];
        }
        assert!(check_plan(&keys, &facts(), &o).is_err());
        // Seventeen layers at once.
        let mut crowd = comp();
        let first = crowd.layers.first().cloned().unwrap();
        for i in 0..render::MAX_RUNNING as u32 {
            let mut l = first.clone();
            l.id = 100 + i;
            crowd.layers.push(l);
        }
        assert!(check_plan(&crowd, &facts(), &o).is_err());
        // ... unless they are muted sound.
        for l in crowd.layers.iter_mut().skip(1) {
            l.kind = LayerKind::Music;
            l.muted = true;
        }
        check_plan(&crowd, &facts(), &o).unwrap();
    }

    #[test]
    fn missing_media_fails_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.mp4");
        std::fs::write(&main, b"x").unwrap();
        let mut c = comp();
        let mut b = c.layers.first().cloned().unwrap();
        b.id = 99;
        b.source = Media::Library {
            key: "Game/other clip.mp4".into(),
        };
        c.layers.push(b);
        let enc = Encoders::default();
        let out = dir.path().join("out.mp4");
        let req = |media: &'static [(Media, PathBuf)]| ExportRequest {
            source: &main,
            composition: &c,
            media,
            preset: Preset::YouTube,
            out: &out,
            encoders: &enc,
            discord_limit_bytes: DISCORD_LIMIT_BYTES,
            pretend_overshoot: 0.0,
        };
        let e = media_files(&req(&[]), &facts()).err().unwrap();
        assert_eq!(e.to_string(), "the clip other clip.mp4 is missing");
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
}
