//! Capture + hardware-encode pipelines that deliver **encoded packets** to a sink.
//!
//! Windows video path (ADR 0002, 0010): Scout's own WGC capture
//! (`klip_platform::GpuCapture`: frames stamped with WGC's capture time, cropped and
//! scaled on the GPU) `→ appsrc → videorate (drop only) → d3d11convert → NV12 → queue →
//! encoder → parser → appsink`, all on one D3D11 device shared through a `GstContext`.
//! If that can't start, `d3d11screencapturesrc` (WGC) takes its place. The output size is
//! fixed when the session starts, so window resizes are scaled/letterboxed instead of
//! changing the encoder's caps mid-stream.
//!
//! The encode half ([`build_encode_chain`]) is public so tests and benchmarks can feed it
//! from an `appsrc` with known frames.
//!
//! Audio ([`audio`]) runs in its own pipeline so a failing device never stops video. All
//! pipelines of a recording share one [`TimeBase`] (clock and base time), so their
//! timestamps are on the same timeline.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

pub mod apps;
pub mod audio;
mod lag;
pub mod props;
pub mod thumbnail;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;

use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use klip_media::{
    AudioParams, Codec, Colorimetry, FrameGrid, MAX_PACKET_BYTES, Nanos, Packet, TrackInfo,
    TrackParams, VideoParams,
};
use klip_probe::{EncoderChoice, EncoderFamily};

pub use klip_probe;

/// Errors from building or running a capture pipeline.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("GStreamer: {0}")]
    Gst(String),
    #[error("element {0} is not installed")]
    MissingElement(String),
    #[error("capture target unavailable: {0}")]
    Target(String),
    #[error("platform: {0}")]
    Platform(#[from] klip_platform::PlatformError),
    #[error("invalid configuration: {0}")]
    Config(String),
}

pub fn gst_err(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Gst(e.to_string())
}

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoTarget {
    PrimaryMonitor,
    /// HMONITOR.
    Monitor(u64),
    /// HWND; client area only.
    Window(u64),
    /// The first stream of [`VideoConfig::portal`], a screen-cast portal session (Linux:
    /// the monitor or window the user picked in the desktop's dialog).
    Portal,
}

/// An open screen-cast portal session, shared by every pipeline of a recording (encoder
/// fallbacks and restarts reconnect to the same stream without asking the user again).
#[derive(Clone)]
pub struct PortalSession(pub Arc<klip_platform::ScreenCast>);

impl std::fmt::Debug for PortalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq for PortalSession {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PortalSession {}

/// Video capture settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoConfig {
    pub target: VideoTarget,
    pub fps: u32,
    /// Output is scaled down (never up) to fit in this box, keeping aspect ratio.
    pub max_width: u32,
    pub max_height: u32,
    pub show_cursor: bool,
    /// Which capture implementation to use.
    pub api: CaptureApi,
    /// Fixed output size instead of fitting the source into the max box (keeps the
    /// encoder's output, and so the replay track, unchanged when a recording switches
    /// between window and monitor capture).
    pub output: Option<(u32, u32)>,
    /// The session [`VideoTarget::Portal`] captures from.
    pub portal: Option<PortalSession>,
}

/// Capture implementation for video.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureApi {
    /// Scout's own WGC capture, falling back to GStreamer's WGC source.
    #[default]
    Auto,
    /// Scout's own WGC capture only (`klip_platform::GpuCapture`).
    Klip,
    /// GStreamer's `d3d11screencapturesrc` in WGC mode.
    GstWgc,
    /// GStreamer's `d3d11screencapturesrc` with DXGI Desktop Duplication (monitors only;
    /// sees exclusive-fullscreen games that WGC may not).
    GstDxgi,
    /// The screen-cast portal + PipeWire (Linux; [`VideoTarget::Portal`]).
    Portal,
}

/// Encoder settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeConfig {
    pub encoder: EncoderChoice,
    pub bitrate_kbps: u32,
    /// Keyframe interval in frames (Scout uses 1 s).
    pub keyframe_interval: u32,
    /// Peak bitrate; `None` = 1.5 x `bitrate_kbps` (the export's size-fit sets it).
    pub max_bitrate_kbps: Option<u32>,
    /// Live capture: the queue before the encoder drops frames when the encoder falls
    /// behind (a capture must never stall). `false` for an export: the feed blocks.
    pub leaky_queue: bool,
}

/// Receives the output of a pipeline. Called on GStreamer streaming threads; must be
/// quick and must not block on I/O.
pub trait PacketSink: Send + Sync + 'static {
    fn on_track_info(&self, track: usize, info: TrackInfo);
    fn on_packet(&self, track: usize, packet: Packet);
    fn on_error(&self, message: String);
    /// Non-fatal problems, e.g. an audio device that failed and now records silence.
    fn on_warning(&self, message: String) {
        tracing::warn!(warning = %message, "capture warning");
    }
}

/// One clock and base time shared by every pipeline of a recording, so buffers from
/// different pipelines (video, audio) carry running times on the same timeline.
#[derive(Clone, Debug)]
pub struct TimeBase {
    clock: gst::Clock,
    base_time: gst::ClockTime,
}

impl TimeBase {
    /// The monotonic system clock, with "now" as time zero.
    pub fn new() -> Result<TimeBase, CaptureError> {
        klip_probe::init().map_err(gst_err)?;
        let clock = gst::SystemClock::obtain();
        let base_time = clock.time();
        Ok(TimeBase { clock, base_time })
    }

    /// Make `pipeline` use this clock and base time (call before starting it).
    pub fn apply(&self, pipeline: &gst::Pipeline) {
        pipeline.use_clock(Some(&self.clock));
        pipeline.set_start_time(gst::ClockTime::NONE);
        pipeline.set_base_time(self.base_time);
    }

    /// Clock time (ns) of running time zero. On Windows the system clock counts
    /// QueryPerformanceCounter time, the same base WASAPI and WGC timestamps use, so this
    /// maps clip times to other QPC-based logs (tests, bookmarks).
    pub fn base_clock_ns(&self) -> u64 {
        self.base_time.nseconds()
    }

    /// Current running time on this timeline.
    pub fn now(&self) -> Nanos {
        let t = self.clock.time().saturating_sub(self.base_time);
        i64::try_from(t.nseconds()).unwrap_or(Nanos::MAX)
    }
}

/// How the appsink callback treats buffer timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    /// Keep them.
    AsIs,
    /// Snap video to a constant-rate grid of this many frames per second (see
    /// [`FrameGrid`]). Only applied to streams without reordering (pts == dts).
    FrameGrid(u32),
    /// Add this offset (e.g. minus an encoder's delay).
    Shift(Nanos),
}

/// Live counters of a session.
#[derive(Debug, Default)]
pub struct CaptureStats {
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    pub keyframes: AtomicU64,
    pub errors: AtomicU64,
    pub warnings: AtomicU64,
    pub callback_panics: AtomicU64,
    /// Video frames whose timestamp grid had to restart (see [`FrameGrid`]).
    pub grid_restarts: AtomicU64,
    /// Frames dropped in front of the encoder because it fell behind (the queue there
    /// leaks rather than stalling capture).
    pub encoder_queue_drops: AtomicU64,
}

/// Scale `(w, h)` down to fit in `(max_w, max_h)`, keeping aspect ratio, never upscaling,
/// rounding to even numbers (NV12), minimum 64x64.
pub fn fit_size(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    let (max_w, max_h) = (max_w.max(64), max_h.max(64));
    let (ow, oh) = if w <= max_w && h <= max_h {
        (w, h)
    } else {
        // Compare w/max_w with h/max_h without floats: w*max_h vs h*max_w.
        let wide = u64::from(w).saturating_mul(u64::from(max_h))
            >= u64::from(h).saturating_mul(u64::from(max_w));
        if wide {
            let nh = u64::from(h)
                .saturating_mul(u64::from(max_w))
                .checked_div(u64::from(w))
                .unwrap_or(0);
            (max_w, u32::try_from(nh).unwrap_or(max_h))
        } else {
            let nw = u64::from(w)
                .saturating_mul(u64::from(max_h))
                .checked_div(u64::from(h))
                .unwrap_or(0);
            (u32::try_from(nw).unwrap_or(max_w), max_h)
        }
    };
    ((ow & !1).max(64), (oh & !1).max(64))
}

pub fn make(name: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| CaptureError::MissingElement(name.to_string()))
}

pub fn capsfilter(caps: &str) -> Result<gst::Element, CaptureError> {
    let el = make("capsfilter")?;
    let caps = gst::Caps::from_str(caps).map_err(gst_err)?;
    if el.find_property("caps").is_none() {
        return Err(CaptureError::Gst("capsfilter has no caps property".into()));
    }
    el.set_property("caps", &caps);
    Ok(el)
}

fn parser_and_caps(codec: Codec) -> Result<(&'static str, &'static str), CaptureError> {
    match codec {
        Codec::H264 => Ok(("h264parse", "video/x-h264,stream-format=avc,alignment=au")),
        Codec::H265 => Ok(("h265parse", "video/x-h265,stream-format=hvc1,alignment=au")),
        Codec::Av1 => Ok((
            "av1parse",
            "video/x-av1,stream-format=obu-stream,alignment=tu",
        )),
        Codec::Aac | Codec::Opus => Err(CaptureError::Config("not a video codec".into())),
    }
}

/// Apply rate control and GOP settings for an encoder family: VBR at the target bitrate
/// with peaks up to 1.5x, one keyframe (IDR) per `keyframe_interval` frames, no
/// B-frames. Unknown properties are skipped with a log line; the required ones (bitrate,
/// keyframe interval) error out.
///
/// Property names: NVENC, Media Foundation and D3D12 verified on an RTX 4080 SUPER with
/// GStreamer 1.28.6; AMF and QSV taken from the GStreamer 1.28 element documentation
/// (no AMD/Intel hardware tested yet, see ROADMAP.md).
fn configure_encoder(el: &gst::Element, cfg: &EncodeConfig) -> Result<(), CaptureError> {
    let kbps = cfg.bitrate_kbps.to_string();
    let max_kbps = cfg
        .max_bitrate_kbps
        .unwrap_or_else(|| {
            cfg.bitrate_kbps
                .saturating_mul(3)
                .checked_div(2)
                .unwrap_or(cfg.bitrate_kbps)
        })
        .max(cfg.bitrate_kbps)
        .to_string();
    let gop = cfg.keyframe_interval.max(1).to_string();
    let req =
        |name: &str, value: &str| props::require(el, name, value).map_err(CaptureError::Config);
    match cfg.encoder.family {
        EncoderFamily::NvencD3d11 | EncoderFamily::NvencCuda => {
            req("bitrate", &kbps)?;
            req("gop-size", &gop)?;
            props::set_str(el, "rc-mode", "vbr");
            props::set_str(el, "max-bitrate", &max_kbps);
            props::set_str(el, "preset", "p4");
            props::set_str(el, "tune", "high-quality");
            props::set_str(el, "bframes", "0");
            props::set_str(el, "spatial-aq", "true");
        }
        EncoderFamily::MediaFoundation => {
            req("bitrate", &kbps)?;
            req("gop-size", &gop)?;
            // Peak-constrained VBR; the default ("uvbr") has no peak limit.
            props::set_str(el, "rc-mode", "pcvbr");
            props::set_str(el, "max-bitrate", &max_kbps);
            props::set_str(el, "bframes", "0");
            props::set_str(el, "low-latency", "true");
        }
        EncoderFamily::D3d12 => {
            req("bitrate", &kbps)?;
            req("gop-size", &gop)?;
            props::set_str(el, "rate-control", "vbr");
            props::set_str(el, "max-bitrate", &max_kbps);
        }
        EncoderFamily::Amf => {
            req("bitrate", &kbps)?;
            req("gop-size", &gop)?;
            // AMF's "vbr" is peak-constrained VBR.
            props::set_str(el, "rate-control", "vbr");
            props::set_str(el, "max-bitrate", &max_kbps);
            props::set_str(el, "b-frames", "0");
        }
        EncoderFamily::Qsv => {
            req("bitrate", &kbps)?;
            req("gop-size", &gop)?;
            props::set_str(el, "rate-control", "vbr");
            props::set_str(el, "max-bitrate", &max_kbps);
            props::set_str(el, "b-frames", "0");
            // Every I-frame an IDR, so each keyframe can start a clip.
            props::set_str(el, "idr-interval", "0");
        }
        EncoderFamily::VaApi => {
            req("bitrate", &kbps)?;
            req("key-int-max", &gop)?;
            props::set_str(el, "b-frames", "0");
        }
        EncoderFamily::OpenH264 => {
            req(
                "bitrate",
                &cfg.bitrate_kbps.saturating_mul(1000).to_string(),
            )?;
            req("gop-size", &gop)?;
        }
    }
    Ok(())
}

/// The first timestamp that entered an encoder, recorded by a pad probe. Some encoders
/// offset their output timestamps (GStreamer's NVENC elements add 1000 hours through the
/// `min-pts` mechanism so B-frame DTS can't go negative); the appsink callback uses this
/// to move packets back onto the capture timeline, which audio shares.
#[derive(Clone, Debug, Default)]
pub struct InputOrigin(Arc<OnceLock<Nanos>>);

impl InputOrigin {
    /// Record the timestamp of the first buffer that passes `pad`.
    pub fn watch(pad: &gst::Pad) -> InputOrigin {
        let origin = InputOrigin::default();
        let cell = origin.0.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(b)) = &info.data
                && let Some(pts) = clock_ns(b.pts())
            {
                let _ = cell.set(pts);
                return gst::PadProbeReturn::Remove;
            }
            gst::PadProbeReturn::Ok
        });
        origin
    }

    pub fn get(&self) -> Option<Nanos> {
        self.0.get().copied()
    }
}

/// The encode half of a video pipeline (see [`build_encode_chain`]).
#[derive(Debug)]
pub struct EncodeChain {
    /// Link your source to this.
    pub input: gst::Element,
    pub appsink: gst_app::AppSink,
    /// First timestamp into the encoder.
    pub origin: InputOrigin,
    /// The leaky queue in front of the encoder.
    queue: gst::Element,
    /// The encoder element.
    pub encoder: gst::Element,
    /// Capsfilters that carry the output size, with their caps minus the size.
    sized: Vec<(gst::Element, String)>,
}

/// Caps `base` at `width x height` with square pixels.
fn sized_caps(base: &str, width: u32, height: u32) -> String {
    format!("{base},width={width},height={height},pixel-aspect-ratio=1/1")
}

/// A capsfilter for `base` at `width x height`, remembered in `sized` so the size can be
/// changed before the first frame (see [`EncodeChain::set_size`]).
fn sized_filter(
    base: &str,
    width: u32,
    height: u32,
    sized: &mut Vec<(gst::Element, String)>,
) -> Result<gst::Element, CaptureError> {
    let el = capsfilter(&sized_caps(base, width, height))?;
    sized.push((el.clone(), base.to_string()));
    Ok(el)
}

/// Point the size-carrying capsfilters of a chain at `width x height`.
fn set_sizes(
    sized: &[(gst::Element, String)],
    width: u32,
    height: u32,
) -> Result<(), CaptureError> {
    for (el, base) in sized {
        let caps = gst::Caps::from_str(&sized_caps(base, width, height)).map_err(gst_err)?;
        el.set_property("caps", &caps);
    }
    Ok(())
}

impl EncodeChain {
    /// Change the output size. Only before the first frame: the encoder's caps are fixed
    /// once it runs.
    pub fn set_size(&self, width: u32, height: u32) -> Result<(), CaptureError> {
        set_sizes(&self.sized, width, height)
    }

    /// The DXGI adapter the encoder runs on, if it reports one (`adapter-luid`).
    pub fn adapter_luid(&self) -> Option<i64> {
        let spec = self.encoder.find_property("adapter-luid")?;
        if spec.value_type() != i64::static_type() {
            return None;
        }
        self.encoder
            .property_value("adapter-luid")
            .get::<i64>()
            .ok()
            .filter(|l| *l != 0)
    }
}

impl EncodeChain {
    /// Route the encoded packets to `sink` as `track`, with timestamps back on the input
    /// timeline (undoing any encoder offset) and then adjusted by `timing`.
    pub fn attach(
        &self,
        track: usize,
        sink: Arc<dyn PacketSink>,
        stats: Arc<CaptureStats>,
        timing: Timing,
    ) {
        let drops = stats.clone();
        // Emitted when the queue is full, right before it drops the incoming frame.
        self.queue.connect("overrun", false, move |_| {
            drops.encoder_queue_drops.fetch_add(1, Ordering::Relaxed);
            None
        });
        attach(
            &self.appsink,
            track,
            sink,
            stats,
            SinkState::new(timing, Some(self.origin.clone())),
        );
    }
}

/// Elements from the chain input to NV12 at `width x height` in the memory the encoder
/// takes. Windows: input is `video/x-raw(memory:D3D11Memory)`.
#[cfg(windows)]
fn convert_front(
    cfg: &EncodeConfig,
    width: u32,
    height: u32,
    sized: &mut Vec<(gst::Element, String)>,
) -> Result<Vec<gst::Element>, CaptureError> {
    let mut chain = vec![make("d3d11convert")?];
    chain.push(sized_filter(
        "video/x-raw(memory:D3D11Memory),format=NV12",
        width,
        height,
        sized,
    )?);
    match cfg.encoder.family {
        EncoderFamily::NvencCuda => {
            chain.push(make("d3d11download")?);
            chain.push(make("cudaupload")?);
        }
        // D3D11 → D3D12 on the GPU (shared texture), no download.
        EncoderFamily::D3d12 => chain.push(make("d3d12upload")?),
        _ if cfg.encoder.needs_system_memory() => chain.push(make("d3d11download")?),
        _ => {}
    }
    Ok(chain)
}

/// Linux: input is system-memory `video/x-raw` (any format the converter takes, e.g.
/// PipeWire's BGRx). Conversion and scaling run on the GPU where the encoder has a way in:
/// OpenGL for NVENC (it takes GL textures; GStreamer's CUDA converters need NVIDIA's
/// runtime compiler `libnvrtc`, which desktops don't have), VA for VA-API; openh264
/// converts on the CPU (I420). `glcolorscale` only scales RGBA textures, hence the
/// conversions around it.
#[cfg(not(windows))]
fn convert_front(
    cfg: &EncodeConfig,
    width: u32,
    height: u32,
    sized: &mut Vec<(gst::Element, String)>,
) -> Result<Vec<gst::Element>, CaptureError> {
    let (w, h) = (width, height);
    Ok(match cfg.encoder.family {
        EncoderFamily::NvencCuda => vec![
            make("glupload")?,
            make("glcolorconvert")?,
            capsfilter("video/x-raw(memory:GLMemory),format=RGBA")?,
            make("glcolorscale")?,
            sized_filter("video/x-raw(memory:GLMemory),format=RGBA", w, h, sized)?,
            make("glcolorconvert")?,
            sized_filter("video/x-raw(memory:GLMemory),format=NV12", w, h, sized)?,
        ],
        EncoderFamily::VaApi => vec![
            make("vapostproc")?,
            sized_filter("video/x-raw(memory:VAMemory),format=NV12", w, h, sized)?,
        ],
        _ => vec![
            make("videoconvertscale")?,
            sized_filter("video/x-raw,format=I420", w, h, sized)?,
        ],
    })
}

/// Build `convert/scale → NV12 (w x h) on the encoder's device → queue → encoder → parser →
/// appsink` inside `bin` (see [`convert_front`] for the input each platform takes).
pub fn build_encode_chain(
    bin: &gst::Pipeline,
    cfg: &EncodeConfig,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<EncodeChain, CaptureError> {
    if width == 0 || height == 0 || fps == 0 || fps > 1000 {
        return Err(CaptureError::Config(format!(
            "bad size/fps {width}x{height}@{fps}"
        )));
    }
    let mut sized = Vec::new();
    let mut chain = convert_front(cfg, width, height, &mut sized)?;
    let convert = chain
        .first()
        .cloned()
        .ok_or_else(|| CaptureError::Gst("empty encode chain".into()))?;
    let queue = make("queue")?;
    props::set_str(&queue, "max-size-buffers", "8");
    props::set_str(&queue, "max-size-bytes", "0");
    props::set_str(&queue, "max-size-time", "0");
    if cfg.leaky_queue {
        props::set_str(&queue, "leaky", "downstream");
    }
    chain.push(queue.clone());
    let encoder = make(&cfg.encoder.element)?;
    if klip_probe::allowed(&cfg.encoder.element).is_none() {
        return Err(CaptureError::Config(format!(
            "{} is not an allowed encoder",
            cfg.encoder.element
        )));
    }
    configure_encoder(&encoder, cfg)?;
    let origin = InputOrigin::watch(
        &encoder
            .static_pad("sink")
            .ok_or_else(|| CaptureError::Gst("encoder has no sink pad".into()))?,
    );
    chain.push(encoder.clone());
    let (parser, out_caps) = parser_and_caps(cfg.encoder.codec)?;
    chain.push(make(parser)?);
    chain.push(capsfilter(out_caps)?);
    let appsink = gst_app::AppSink::builder()
        .sync(false)
        .enable_last_sample(false)
        .max_buffers(0)
        .build();
    chain.push(appsink.clone().upcast());
    bin.add_many(&chain).map_err(gst_err)?;
    gst::Element::link_many(&chain).map_err(gst_err)?;
    Ok(EncodeChain {
        input: convert,
        appsink,
        origin,
        queue,
        encoder,
        sized,
    })
}

fn clock_ns(t: Option<gst::ClockTime>) -> Option<Nanos> {
    t.and_then(|t| i64::try_from(t.nseconds()).ok())
}

fn colorimetry_from_caps(s: &gst::StructureRef) -> Option<Colorimetry> {
    let text = s.get::<&str>("colorimetry").ok()?;
    let c = gst_video::VideoColorimetry::from_str(text).ok()?;
    let iso = |v: u32| u16::try_from(v).ok();
    Some(Colorimetry {
        primaries: iso(c.primaries().to_iso())?,
        transfer: iso(c.transfer().to_iso())?,
        matrix: iso(c.matrix().to_iso())?,
        full_range: c.range() == gst_video::VideoColorRange::Range0_255,
    })
}

/// Describe an encoded stream from its parser caps: H.264/H.265/AV1 video or raw
/// (not ADTS) AAC.
pub fn track_info_from_caps(caps: &gst::CapsRef) -> Result<TrackInfo, String> {
    track_info_with_frame(caps, None)
}

/// Like [`track_info_from_caps`], with the stream's first frame for codecs whose caps
/// don't carry the configuration (AV1 from `av1parse`: `av1C` is built from the
/// sequence header in the first keyframe).
pub fn track_info_with_frame(
    caps: &gst::CapsRef,
    first_frame: Option<&[u8]>,
) -> Result<TrackInfo, String> {
    let s = caps.structure(0).ok_or("empty caps")?;
    let codec = match s.name().as_str() {
        "video/x-h264" => Codec::H264,
        "video/x-h265" => Codec::H265,
        "video/x-av1" => Codec::Av1,
        "audio/mpeg" => return aac_track_info(s),
        other => return Err(format!("unsupported caps {other}")),
    };
    let codec_data = match s.get::<gst::Buffer>("codec_data") {
        Ok(b) => Bytes::copy_from_slice(b.map_readable().map_err(|e| e.to_string())?.as_slice()),
        Err(_) if codec == Codec::Av1 => {
            let frame = first_frame.ok_or("AV1 caps have no codec_data")?;
            Bytes::from(
                klip_media::av1::config_record(frame)
                    .ok_or("no AV1 sequence header in the first frame")?,
            )
        }
        Err(_) => return Err("caps have no codec_data".into()),
    };
    let width = s.get::<i32>("width").map_err(|e| e.to_string())?;
    let height = s.get::<i32>("height").map_err(|e| e.to_string())?;
    let info = TrackInfo {
        codec,
        codec_data,
        params: TrackParams::Video(VideoParams {
            width: u32::try_from(width).map_err(|e| e.to_string())?,
            height: u32::try_from(height).map_err(|e| e.to_string())?,
            colorimetry: colorimetry_from_caps(s),
        }),
        bitrate: None,
    };
    info.validate().map_err(|e| e.to_string())?;
    Ok(info)
}

/// Log an H.264 stream that signals a lower level than its size and frame rate need
/// (the AMD tester's AMF at 2560x1440 / 144 fps once signalled 5.0; 5.2 is needed). Most
/// players don't mind; the log tells a tester's report what the encoder did.
fn warn_low_level(caps: &gst::CapsRef, info: &TrackInfo) {
    let (Codec::H264, TrackParams::Video(v)) = (&info.codec, &info.params) else {
        return;
    };
    let Some(fps) = caps
        .structure(0)
        .and_then(|s| s.get::<gst::Fraction>("framerate").ok())
    else {
        return;
    };
    let (Ok(num), Ok(den)) = (u32::try_from(fps.numer()), u32::try_from(fps.denom())) else {
        return;
    };
    let have = klip_media::h264::signalled_level(&info.codec_data);
    let need = klip_media::h264::needed_level(v.width, v.height, num, den);
    if let (Some(have), Some(need)) = (have, need)
        && have < need
    {
        tracing::warn!(
            signalled = have,
            needed = need,
            width = v.width,
            height = v.height,
            fps = %format!("{num}/{den}"),
            "the encoder signals a lower H.264 level than the stream needs"
        );
    }
}

fn aac_track_info(s: &gst::StructureRef) -> Result<TrackInfo, String> {
    if s.get::<i32>("mpegversion").ok() != Some(4)
        || s.get::<&str>("stream-format").ok() != Some("raw")
    {
        return Err("only raw MPEG-4 AAC is supported".into());
    }
    let codec_data = s
        .get::<gst::Buffer>("codec_data")
        .map_err(|_| "caps have no codec_data".to_string())?;
    let map = codec_data.map_readable().map_err(|e| e.to_string())?;
    let rate = s.get::<i32>("rate").map_err(|e| e.to_string())?;
    let channels = s.get::<i32>("channels").map_err(|e| e.to_string())?;
    let info = TrackInfo {
        codec: Codec::Aac,
        codec_data: Bytes::copy_from_slice(map.as_slice()),
        params: TrackParams::Audio(AudioParams {
            sample_rate: u32::try_from(rate).map_err(|e| e.to_string())?,
            channels: u16::try_from(channels).map_err(|e| e.to_string())?,
        }),
        bitrate: s
            .get::<i32>("bitrate")
            .ok()
            .and_then(|b| u32::try_from(b).ok()),
    };
    info.validate().map_err(|e| e.to_string())?;
    Ok(info)
}

struct SinkState {
    caps: Option<gst::Caps>,
    timing: Timing,
    grid: Option<FrameGrid>,
    origin: Option<InputOrigin>,
    /// Shift back onto the input timeline, fixed by the first packet.
    restore: Option<Nanos>,
}

impl SinkState {
    fn new(timing: Timing, origin: Option<InputOrigin>) -> SinkState {
        let grid = match timing {
            Timing::FrameGrid(fps) => FrameGrid::new(fps),
            Timing::AsIs | Timing::Shift(_) => None,
        };
        SinkState {
            caps: None,
            timing,
            grid,
            origin,
            restore: None,
        }
    }

    /// Adjusted (pts, dts).
    fn retime(&mut self, pts: Nanos, dts: Nanos, stats: &CaptureStats) -> (Nanos, Nanos) {
        if let Some(origin) = &self.origin {
            let shift = match self.restore {
                Some(s) => s,
                None => {
                    let s = match origin.get() {
                        Some(first_in) => first_in.saturating_sub(pts),
                        None => {
                            tracing::warn!("encoder input time unknown; keeping its timestamps");
                            0
                        }
                    };
                    tracing::debug!(
                        shift = s,
                        "encoder timestamps moved onto the capture timeline"
                    );
                    self.restore = Some(s);
                    s
                }
            };
            return self.adjust(pts.saturating_add(shift), dts.saturating_add(shift), stats);
        }
        self.adjust(pts, dts, stats)
    }

    fn adjust(&mut self, pts: Nanos, dts: Nanos, stats: &CaptureStats) -> (Nanos, Nanos) {
        match (self.timing, self.grid.as_mut()) {
            (Timing::Shift(by), _) => (pts.saturating_add(by), dts.saturating_add(by)),
            (Timing::FrameGrid(_), Some(grid)) if pts == dts => {
                let before = grid.restarts();
                let t = grid.snap(pts);
                if grid.restarts() != before {
                    stats.grid_restarts.fetch_add(1, Ordering::Relaxed);
                }
                (t, t)
            }
            _ => (pts, dts),
        }
    }
}

fn handle_sample(
    appsink: &gst_app::AppSink,
    track: usize,
    sink: &dyn PacketSink,
    stats: &CaptureStats,
    state: &Mutex<SinkState>,
) -> Result<(), String> {
    let sample = appsink.pull_sample().map_err(|e| e.to_string())?;
    let buffer = sample.buffer().ok_or("sample without buffer")?;
    if buffer.size() > MAX_PACKET_BYTES {
        return Err(format!("encoded frame too large ({} bytes)", buffer.size()));
    }
    let map = buffer.map_readable().map_err(|e| e.to_string())?;
    if let Some(caps) = sample.caps() {
        let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.caps.as_ref().map(|c| c.as_ref()) != Some(caps) {
            let info = track_info_with_frame(caps, Some(map.as_slice()))?;
            warn_low_level(caps, &info);
            st.caps = Some(caps.to_owned());
            sink.on_track_info(track, info);
        }
    }
    let pts = clock_ns(buffer.pts()).ok_or("buffer without timestamp")?;
    let dts = clock_ns(buffer.dts()).unwrap_or(pts);
    let (pts, dts) = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retime(pts, dts, stats);
    let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
    let packet = Packet {
        pts,
        dts,
        duration: clock_ns(buffer.duration()).unwrap_or(0),
        keyframe,
        data: Bytes::copy_from_slice(map.as_slice()),
    };
    stats.packets.fetch_add(1, Ordering::Relaxed);
    stats
        .bytes
        .fetch_add(u64::try_from(map.size()).unwrap_or(0), Ordering::Relaxed);
    if keyframe {
        stats.keyframes.fetch_add(1, Ordering::Relaxed);
    }
    sink.on_packet(track, packet);
    Ok(())
}

/// Route an appsink's samples to `sink` as packets of `track`, timestamps as they are.
/// Panics inside the callback are caught and counted; the stream keeps flowing.
pub fn attach_sink(
    appsink: &gst_app::AppSink,
    track: usize,
    sink: Arc<dyn PacketSink>,
    stats: Arc<CaptureStats>,
) {
    attach_sink_with(appsink, track, sink, stats, Timing::AsIs);
}

/// [`attach_sink`] with a timestamp adjustment.
pub fn attach_sink_with(
    appsink: &gst_app::AppSink,
    track: usize,
    sink: Arc<dyn PacketSink>,
    stats: Arc<CaptureStats>,
    timing: Timing,
) {
    attach(appsink, track, sink, stats, SinkState::new(timing, None));
}

fn attach(
    appsink: &gst_app::AppSink,
    track: usize,
    sink: Arc<dyn PacketSink>,
    stats: Arc<CaptureStats>,
    state: SinkState,
) {
    let state = Mutex::new(state);
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |appsink| {
                let res = catch_unwind(AssertUnwindSafe(|| {
                    handle_sample(appsink, track, sink.as_ref(), &stats, &state)
                }));
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        stats.errors.fetch_add(1, Ordering::Relaxed);
                        sink.on_error(e);
                    }
                    Err(_) => {
                        stats.callback_panics.fetch_add(1, Ordering::Relaxed);
                        sink.on_error("internal error while handling a frame".into());
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// A running capture pipeline.
pub struct CaptureSession {
    pipeline: gst::Pipeline,
    stop: Arc<AtomicBool>,
    bus_thread: Option<JoinHandle<()>>,
    stats: Arc<CaptureStats>,
    output: (u32, u32),
    /// Scout's own capture feeding the pipeline (stopped before the pipeline).
    #[cfg(windows)]
    gpu: Option<klip_platform::GpuCapture>,
    /// Keeps the screen-cast session open while its stream is captured.
    portal: Option<PortalSession>,
    /// Which implementation captures.
    api: CaptureApi,
}

impl std::fmt::Debug for CaptureSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSession")
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

/// Report it as a capture error when the desktop ends the screen-cast session (the user
/// stopped sharing): the stream then just stops delivering frames.
fn watch_portal(
    portal: &PortalSession,
    stop: &Arc<AtomicBool>,
    sink: Arc<dyn PacketSink>,
    stats: Arc<CaptureStats>,
) {
    let (portal, stop) = (portal.clone(), stop.clone());
    let spawned = std::thread::Builder::new()
        .name("klip-portal-watch".into())
        .spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| {
                while !stop.load(Ordering::Relaxed) {
                    if portal.0.is_closed() {
                        stats.errors.fetch_add(1, Ordering::Relaxed);
                        sink.on_error("screen sharing was stopped from the desktop".into());
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            }));
            if r.is_err() {
                tracing::error!("the screen-cast watcher panicked");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "cannot watch the screen-cast session");
    }
}

/// How long a screen-cast stream may take to deliver its first frame (the compositor sends
/// one as soon as the stream connects).
const PORTAL_FIRST_FRAME: std::time::Duration = std::time::Duration::from_secs(5);

/// Reference caps of the arrival-time meta on captured frames.
const ARRIVAL_META: &str = "timestamp/x-klip-arrival";

/// How often the WGC source looks for a new frame, relative to the capture rate. The
/// source stamps and hands over the newest frame once per slot of its frame-rate grid, so
/// at the capture rate a frame could be up to a whole frame old, by an amount that differs
/// per recording. Polling faster keeps that under a few ms; `videorate` then brings the
/// rate back down by picking the frame nearest each output slot.
const SOURCE_RATE_FACTOR: u32 = 8;
const SOURCE_RATE_MIN: u32 = 240;
const SOURCE_RATE_MAX: u32 = 1000;

/// Stamp `src`'s buffers with their arrival time on `tb`'s timeline, and remember that time
/// in a `ReferenceTimestampMeta` (rate conversion downstream rewrites the timestamps).
fn restamp_on_arrival(
    src: &gst::Element,
    tb: &TimeBase,
    latency: Nanos,
) -> Result<(), CaptureError> {
    let pad = src
        .static_pad("src")
        .ok_or_else(|| CaptureError::Gst("capture source has no src pad".into()))?;
    let tb = tb.clone();
    let caps = gst::Caps::new_empty_simple(ARRIVAL_META);
    let last = Mutex::new(Nanos::MIN);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() {
            let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
            // Strictly increasing even if two frames arrive within one clock tick.
            let now = tb.now().saturating_sub(latency).max(last.saturating_add(1));
            *last = now;
            let t = gst::ClockTime::from_nseconds(u64::try_from(now).unwrap_or(0));
            let b = buffer.make_mut();
            b.set_pts(t);
            b.set_dts(gst::ClockTime::NONE);
            gst::ReferenceTimestampMeta::add(b, &caps, t, gst::ClockTime::NONE);
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

/// How long after a game finished a frame a screen-cast portal delivers it, subtracted
/// from the arrival time. Measured with the test game (`m2_audio.rs` stamp latency, arrival
/// stamps): KDE Plasma 6.7.5 Wayland, NVIDIA 615.71, 3840x2160 @ 165 Hz: +27.6 to +27.9 ms
/// (system memory), +32.7 ms (DMA-BUF); 24 ms brings it to about +4 ms, like Windows.
/// Other desktops are unmeasured (0). `KLIP_VIDEO_LATENCY_MS` overrides it.
fn compositor_latency() -> Nanos {
    if let Some(ms) = std::env::var("KLIP_VIDEO_LATENCY_MS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
    {
        return ms.clamp(0, 500).saturating_mul(1_000_000);
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if desktop.split(':').any(|d| d.eq_ignore_ascii_case("KDE")) {
        24_000_000
    } else {
        0
    }
}

/// Stamp portal frames: on arrival minus [`compositor_latency`] (the compositor's own stamp
/// is the previous refresh's presentation time, not when the content was drawn; ADR 0012).
/// `KLIP_CAPTURE_STAMPS=source` keeps `pipewiresrc`'s timestamps instead (A/B runs).
fn stamp_portal_frames(src: &gst::Element, tb: &TimeBase) -> Result<(), CaptureError> {
    if std::env::var("KLIP_CAPTURE_STAMPS").is_ok_and(|v| v == "source") {
        return keep_source_times(src, tb);
    }
    restamp_on_arrival(src, tb, compositor_latency())
}

/// Keep each frame's timestamp from `src` (arrival time on `tb` if it has none) in the
/// arrival-time meta, strictly increasing, for [`snap_kept_frames`].
fn keep_source_times(src: &gst::Element, tb: &TimeBase) -> Result<(), CaptureError> {
    let pad = src
        .static_pad("src")
        .ok_or_else(|| CaptureError::Gst("capture source has no src pad".into()))?;
    let tb = tb.clone();
    let caps = gst::Caps::new_empty_simple(ARRIVAL_META);
    let last = Mutex::new(Nanos::MIN);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() {
            let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
            let stamped = clock_ns(buffer.pts()).unwrap_or_else(|| tb.now());
            let now = stamped.max(last.saturating_add(1));
            *last = now;
            let t = gst::ClockTime::from_nseconds(u64::try_from(now).unwrap_or(0));
            let b = buffer.make_mut();
            b.set_pts(t);
            b.set_dts(gst::ClockTime::NONE);
            gst::ReferenceTimestampMeta::add(b, &caps, t, gst::ClockTime::NONE);
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

/// After rate conversion: give each kept frame its arrival time back, snapped to the
/// capture rate's frame grid (constant frame rate, phase following the arrivals).
fn snap_kept_frames(pad: &gst::Pad, fps: u32, stats: Arc<CaptureStats>) {
    let grid = Mutex::new(FrameGrid::new(fps));
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() else {
            return gst::PadProbeReturn::Ok;
        };
        let arrival = buffer
            .iter_meta::<gst::ReferenceTimestampMeta>()
            .find(|m| {
                m.reference()
                    .structure(0)
                    .is_some_and(|s| s.name() == ARRIVAL_META)
            })
            .and_then(|m| i64::try_from(m.timestamp().nseconds()).ok());
        let mut grid = grid.lock().unwrap_or_else(PoisonError::into_inner);
        if let (Some(arrival), Some(g)) = (arrival, grid.as_mut()) {
            let before = g.restarts();
            let t = g.snap(arrival);
            if g.restarts() != before {
                stats.grid_restarts.fetch_add(1, Ordering::Relaxed);
            }
            let b = buffer.make_mut();
            b.set_pts(gst::ClockTime::from_nseconds(u64::try_from(t).unwrap_or(0)));
            b.set_dts(gst::ClockTime::NONE);
        }
        gst::PadProbeReturn::Ok
    });
}

fn source_size(target: VideoTarget) -> Result<(u32, u32, VideoTarget), CaptureError> {
    match target {
        VideoTarget::Window(h) => {
            let w = klip_platform::window_info(h)?;
            if w.minimized || w.client_width == 0 || w.client_height == 0 {
                return Err(CaptureError::Target(
                    "window is minimised or has no client area".into(),
                ));
            }
            Ok((w.client_width, w.client_height, target))
        }
        VideoTarget::Monitor(h) => {
            let m = klip_platform::list_monitors()?
                .into_iter()
                .find(|m| m.handle == h)
                .ok_or_else(|| CaptureError::Target("monitor not found".into()))?;
            Ok((m.width, m.height, target))
        }
        VideoTarget::PrimaryMonitor => {
            let m = klip_platform::list_monitors()?
                .into_iter()
                .find(|m| m.primary)
                .ok_or_else(|| CaptureError::Target("no primary monitor".into()))?;
            Ok((m.width, m.height, VideoTarget::Monitor(m.handle)))
        }
        // Sized from the stream's caps (see `start_portal`).
        VideoTarget::Portal => Err(CaptureError::Target(
            "portal streams have no size before they start".into(),
        )),
    }
}

/// Feeds frames from Scout's own capture into the pipeline's `appsrc`, stamped on the
/// recording's timeline.
#[cfg(windows)]
struct AppsrcFeed {
    appsrc: gst_app::AppSrc,
    /// Clock time (ns) of running time zero.
    base_ns: i64,
    last: Mutex<Nanos>,
    sink: Arc<dyn PacketSink>,
    stats: Arc<CaptureStats>,
    ended: AtomicBool,
}

#[cfg(windows)]
impl klip_platform::GpuFrameSink for AppsrcFeed {
    fn frame(&self, mut buffer: gst::Buffer, qpc_ns: i64) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let t = qpc_ns.saturating_sub(self.base_ns);
        // Before the recording started, or not after the previous frame: skip.
        if t < 0 || t <= *last {
            return;
        }
        *last = t;
        drop(last);
        let pts = gst::ClockTime::from_nseconds(u64::try_from(t).unwrap_or(0));
        let b = buffer.make_mut();
        b.set_pts(pts);
        b.set_dts(gst::ClockTime::NONE);
        b.set_duration(gst::ClockTime::NONE);
        let caps = gst::Caps::new_empty_simple(ARRIVAL_META);
        gst::ReferenceTimestampMeta::add(b, &caps, pts, gst::ClockTime::NONE);
        // Flushing (not playing yet / stopping) and full-queue drops are fine to ignore.
        let _ = self.appsrc.push_buffer(buffer);
    }

    fn ended(&self, reason: String) {
        if !self.ended.swap(true, Ordering::Relaxed) {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            self.sink.on_error(format!("capture: {reason}"));
        }
    }
}

/// The monitor that shows `target` (a window: the one it is mostly on, at start).
#[cfg(windows)]
fn target_monitor(target: VideoTarget) -> Option<klip_platform::MonitorInfo> {
    let monitors = klip_platform::list_monitors().ok()?;
    let handle = match target {
        VideoTarget::PrimaryMonitor => monitors.iter().find(|m| m.primary)?.handle,
        VideoTarget::Monitor(h) => h,
        // A window off every monitor gets the primary's levels (WindowInfo::display).
        VideoTarget::Window(h) => {
            klip_platform::window_info(h)
                .ok()?
                .display(&monitors)?
                .handle
        }
        VideoTarget::Portal => return None,
    };
    monitors.into_iter().find(|m| m.handle == handle)
}

/// HDR settings for the capture when `monitor` runs in Windows HDR mode. Unknown levels
/// default to 80 nits SDR white (Windows' minimum) and a 1000-nit peak.
#[cfg(windows)]
fn hdr_display(monitor: &klip_platform::MonitorInfo) -> Option<klip_platform::HdrDisplay> {
    monitor.hdr.then(|| klip_platform::HdrDisplay {
        sdr_white_nits: monitor.sdr_white_nits.unwrap_or(80.0),
        peak_nits: monitor.max_nits.unwrap_or(1000.0),
    })
}

impl CaptureSession {
    /// Build and start a screen/window capture session (Windows). Packets go to track 0.
    pub fn start(
        video: &VideoConfig,
        encode: &EncodeConfig,
        sink: Arc<dyn PacketSink>,
        timebase: Option<&TimeBase>,
    ) -> Result<CaptureSession, CaptureError> {
        klip_probe::init().map_err(gst_err)?;
        if video.fps == 0 || video.fps > 1000 {
            return Err(CaptureError::Config(format!("bad fps {}", video.fps)));
        }
        if video.target == VideoTarget::Portal {
            return Self::start_portal(video, encode, sink, timebase);
        }
        let (src_w, src_h, target) = source_size(video.target)?;
        let (out_w, out_h) = match video.output {
            Some((w, h)) if w >= 64 && h >= 64 && w <= 8192 && h <= 8192 => (w & !1, h & !1),
            Some(o) => return Err(CaptureError::Config(format!("bad output size {o:?}"))),
            None => fit_size(src_w, src_h, video.max_width, video.max_height),
        };
        #[cfg(windows)]
        if matches!(video.api, CaptureApi::Auto | CaptureApi::Klip) {
            match Self::start_klip(
                video,
                encode,
                sink.clone(),
                timebase,
                target,
                (out_w, out_h),
            ) {
                Ok(s) => return Ok(s),
                Err(e) if video.api == CaptureApi::Auto => {
                    tracing::warn!(error = %e, "own capture failed; using GStreamer's WGC source");
                }
                Err(e) => return Err(e),
            }
        }
        Self::start_gst_wgc(video, encode, sink, timebase, target, (out_w, out_h))
    }

    /// Scout's own WGC capture into an `appsrc` (see the crate docs).
    #[cfg(windows)]
    fn start_klip(
        video: &VideoConfig,
        encode: &EncodeConfig,
        sink: Arc<dyn PacketSink>,
        timebase: Option<&TimeBase>,
        target: VideoTarget,
        (out_w, out_h): (u32, u32),
    ) -> Result<CaptureSession, CaptureError> {
        let own_tb;
        let tb = match timebase {
            Some(t) => t,
            None => {
                own_tb = TimeBase::new()?;
                &own_tb
            }
        };
        let gpu_target = match target {
            VideoTarget::Window(h) => klip_platform::GpuCaptureTarget::Window(h),
            VideoTarget::Monitor(h) => klip_platform::GpuCaptureTarget::Monitor(h),
            VideoTarget::PrimaryMonitor | VideoTarget::Portal => {
                return Err(CaptureError::Target("no monitor handle".into()));
            }
        };
        let pipeline = gst::Pipeline::with_name("klip-capture");
        let appsrc = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .do_timestamp(false)
            .max_buffers(4)
            .block(false)
            .build();
        props::set_str(appsrc.upcast_ref(), "leaky-type", "downstream");
        let videorate = make("videorate")?;
        // Never invent frames; keep the one nearest each output slot.
        props::set_str(&videorate, "drop-only", "true");
        let rate = capsfilter(&format!(
            "video/x-raw(memory:D3D11Memory),framerate={}/1",
            video.fps
        ))?;
        pipeline
            .add_many([appsrc.upcast_ref(), &videorate, &rate])
            .map_err(gst_err)?;
        gst::Element::link_many([appsrc.upcast_ref(), &videorate, &rate]).map_err(gst_err)?;
        let chain = build_encode_chain(&pipeline, encode, out_w, out_h, video.fps)?;
        rate.link(&chain.input).map_err(gst_err)?;
        // Capture on the encoder's GPU (no copy between GPUs); encoders that don't say
        // which GPU they use get the monitor's.
        let monitor = target_monitor(target);
        let adapter_luid = chain
            .adapter_luid()
            .or_else(|| monitor.as_ref().and_then(|m| m.adapter_luid));
        let hdr = monitor.as_ref().and_then(hdr_display);
        let stats = Arc::new(CaptureStats::default());
        let feed = Arc::new(AppsrcFeed {
            appsrc: appsrc.clone(),
            base_ns: i64::try_from(tb.base_clock_ns()).unwrap_or(i64::MAX),
            last: Mutex::new(Nanos::MIN),
            sink: sink.clone(),
            stats: stats.clone(),
            ended: AtomicBool::new(false),
        });
        let gpu = klip_platform::GpuCapture::start(
            &klip_platform::GpuCaptureConfig {
                target: gpu_target,
                adapter_luid,
                show_cursor: video.show_cursor,
                width: out_w,
                height: out_h,
                hdr,
                fps: video.fps,
            },
            feed,
        )?;
        let mut caps = gpu.caps().clone();
        if let Some(c) = caps.get_mut() {
            c.set("framerate", gst::Fraction::new(0, 1));
        }
        appsrc.set_caps(Some(&caps));
        pipeline.set_context(&gpu.context());
        let pad = rate
            .static_pad("src")
            .ok_or_else(|| CaptureError::Gst("capsfilter has no src pad".into()))?;
        snap_kept_frames(&pad, video.fps, stats.clone());
        chain.attach(0, sink.clone(), stats.clone(), Timing::AsIs);
        let mut session = Self::run(pipeline, sink, stats, (out_w, out_h), Some(tb))?;
        session.gpu = Some(gpu);
        session.api = CaptureApi::Klip;
        tracing::info!(width = out_w, height = out_h, fps = video.fps, encoder = %encode.encoder.element, adapter = ?adapter_luid, hdr = hdr.is_some(), "capture started (own WGC)");
        Ok(session)
    }

    /// GStreamer's `d3d11screencapturesrc` in WGC mode.
    fn start_gst_wgc(
        video: &VideoConfig,
        encode: &EncodeConfig,
        sink: Arc<dyn PacketSink>,
        timebase: Option<&TimeBase>,
        target: VideoTarget,
        (out_w, out_h): (u32, u32),
    ) -> Result<CaptureSession, CaptureError> {
        let dxgi = video.api == CaptureApi::GstDxgi;
        if dxgi && matches!(target, VideoTarget::Window(_)) {
            return Err(CaptureError::Config(
                "DXGI desktop duplication captures monitors only".into(),
            ));
        }
        let pipeline = gst::Pipeline::with_name("klip-capture");
        // DXGI: the D3D12 source, which samples an HDR desktop in scRGB at the SDR white
        // level (SDR content exact; the D3D11 source's DXGI mode washes it out: measured
        // 78/115 for 60/90 on a 140-nit SDR-white monitor). Its frames go to the D3D11
        // chain through d3d12download.
        let d3d12 = dxgi && gst::ElementFactory::find("d3d12screencapturesrc").is_some();
        let src = make(if d3d12 {
            "d3d12screencapturesrc"
        } else {
            "d3d11screencapturesrc"
        })?;
        props::require(&src, "capture-api", if dxgi { "dxgi" } else { "wgc" })
            .map_err(CaptureError::Config)?;
        props::set_str(&src, "show-border", "false");
        props::set_str(
            &src,
            "show-cursor",
            if video.show_cursor { "true" } else { "false" },
        );
        match target {
            VideoTarget::Window(h) => {
                props::require(&src, "window-handle", &h.to_string())
                    .map_err(CaptureError::Config)?;
                props::set_str(&src, "window-capture-mode", "client");
            }
            VideoTarget::Monitor(h) => {
                props::require(&src, "monitor-handle", &h.to_string())
                    .map_err(CaptureError::Config)?;
            }
            VideoTarget::PrimaryMonitor => {}
            VideoTarget::Portal => {
                return Err(CaptureError::Config(
                    "portal streams are captured with pipewiresrc".into(),
                ));
            }
        }
        if let Some(tb) = timebase {
            restamp_on_arrival(&src, tb, 0)?;
        }
        let source_rate = video
            .fps
            .saturating_mul(SOURCE_RATE_FACTOR)
            .clamp(SOURCE_RATE_MIN, SOURCE_RATE_MAX);
        let memory = if d3d12 { "D3D12Memory" } else { "D3D11Memory" };
        let poll = capsfilter(&format!(
            "video/x-raw(memory:{memory}),framerate={source_rate}/1"
        ))?;
        let mut elements = vec![src.clone(), poll];
        if d3d12 {
            elements.push(make("d3d12download")?);
            elements.push(capsfilter("video/x-raw(memory:D3D11Memory)")?);
        }
        let videorate = make("videorate")?;
        // Never invent frames; keep the one nearest each output slot.
        props::set_str(&videorate, "drop-only", "true");
        let rate = capsfilter(&format!(
            "video/x-raw(memory:D3D11Memory),framerate={}/1",
            video.fps
        ))?;
        elements.push(videorate);
        elements.push(rate.clone());
        pipeline.add_many(&elements).map_err(gst_err)?;
        gst::Element::link_many(&elements).map_err(gst_err)?;
        let chain = build_encode_chain(&pipeline, encode, out_w, out_h, video.fps)?;
        rate.link(&chain.input).map_err(gst_err)?;

        let stats = Arc::new(CaptureStats::default());
        if timebase.is_some() {
            let pad = rate
                .static_pad("src")
                .ok_or_else(|| CaptureError::Gst("capsfilter has no src pad".into()))?;
            snap_kept_frames(&pad, video.fps, stats.clone());
        }
        chain.attach(0, sink.clone(), stats.clone(), Timing::AsIs);
        let mut session = Self::run(pipeline, sink, stats, (out_w, out_h), timebase)?;
        session.api = if dxgi {
            CaptureApi::GstDxgi
        } else {
            CaptureApi::GstWgc
        };
        tracing::info!(width = out_w, height = out_h, fps = video.fps, encoder = %encode.encoder.element, api = ?session.api, "capture started (GStreamer source)");
        Ok(session)
    }

    /// A screen-cast portal stream: `pipewiresrc` (system memory) → `videorate` → the
    /// encode chain. The output size comes from the stream's first caps (unless pinned):
    /// a probe sizes the chain as they pass, before any frame gets there.
    fn start_portal(
        video: &VideoConfig,
        encode: &EncodeConfig,
        sink: Arc<dyn PacketSink>,
        timebase: Option<&TimeBase>,
    ) -> Result<CaptureSession, CaptureError> {
        let portal = video
            .portal
            .clone()
            .ok_or_else(|| CaptureError::Config("no screen-cast session".into()))?;
        let stream = portal
            .0
            .streams()
            .first()
            .cloned()
            .ok_or_else(|| CaptureError::Target("screen-cast session has no stream".into()))?;
        let pipeline = gst::Pipeline::with_name("klip-capture");
        let src = make("pipewiresrc")?;
        props::require(&src, "fd", &portal.0.pipewire_fd().to_string())
            .map_err(CaptureError::Config)?;
        props::require(&src, "path", &stream.node_id.to_string()).map_err(CaptureError::Config)?;
        // Leave `on-disconnect` at "none": its "error"/"eos" modes fire on every buffer
        // renegotiation (all buffers are removed then too; KWin renegotiates right after
        // the first frame), not only when the stream goes away.
        props::set_str(&src, "client-name", "Scout");
        if let Some(tb) = timebase {
            stamp_portal_frames(&src, tb)?;
        }
        // System memory: every GPU path below can upload it, and the compositor finishes
        // each frame before handing it over. DMA-BUF (`KLIP_PORTAL_CAPS=
        // video/x-raw(memory:DMABuf)`) halves the CPU cost but, on NVIDIA (no implicit
        // sync), repeated stale frames in the M1 test; see ADR 0012.
        let raw = capsfilter(
            &std::env::var("KLIP_PORTAL_CAPS").unwrap_or_else(|_| "video/x-raw".into()),
        )?;
        let videorate = make("videorate")?;
        // Never invent frames; keep the one nearest each output slot.
        props::set_str(&videorate, "drop-only", "true");
        let rate = capsfilter(&format!("video/x-raw(ANY),framerate={}/1", video.fps))?;
        let mut head = vec![src.clone(), raw];
        if std::env::var("KLIP_PORTAL_QUEUE").is_ok() {
            let q = make("queue")?;
            props::set_str(&q, "max-size-buffers", "3");
            props::set_str(&q, "max-size-bytes", "0");
            props::set_str(&q, "max-size-time", "0");
            props::set_str(&q, "leaky", "downstream");
            head.push(q);
        }
        head.extend([videorate, rate.clone()]);
        pipeline.add_many(&head).map_err(gst_err)?;
        gst::Element::link_many(&head).map_err(gst_err)?;
        let pinned = match video.output {
            Some((w, h)) if w >= 64 && h >= 64 && w <= 8192 && h <= 8192 => Some((w & !1, h & !1)),
            Some(o) => return Err(CaptureError::Config(format!("bad output size {o:?}"))),
            None => None,
        };
        // Built at a provisional size; the first caps from the stream set the real one
        // before any frame reaches the chain.
        let (w0, h0) = pinned.unwrap_or((video.max_width.max(64), video.max_height.max(64)));
        let chain = build_encode_chain(&pipeline, encode, w0 & !1, h0 & !1, video.fps)?;
        rate.link(&chain.input).map_err(gst_err)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<((u32, u32), (u32, u32))>(1);
        let sized = chain.sized.clone();
        let max = (video.max_width, video.max_height);
        src.static_pad("src")
            .ok_or_else(|| CaptureError::Gst("pipewiresrc has no src pad".into()))?
            .add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                let Some(gst::PadProbeData::Event(ev)) = &info.data else {
                    return gst::PadProbeReturn::Ok;
                };
                let gst::EventView::Caps(c) = ev.view() else {
                    return gst::PadProbeReturn::Ok;
                };
                let Some(size) = c.caps().structure(0).and_then(|s| {
                    let w = u32::try_from(s.get::<i32>("width").ok()?).ok()?;
                    let h = u32::try_from(s.get::<i32>("height").ok()?).ok()?;
                    Some((w, h))
                }) else {
                    return gst::PadProbeReturn::Ok;
                };
                // Later size changes (a resized window) are scaled to this first size.
                let out = pinned.unwrap_or_else(|| fit_size(size.0, size.1, max.0, max.1));
                if let Err(e) = set_sizes(&sized, out.0, out.1) {
                    tracing::warn!(error = %e, "could not size the encode chain");
                }
                let _ = tx.try_send((size, out));
                gst::PadProbeReturn::Remove
            });
        let stats = Arc::new(CaptureStats::default());
        if timebase.is_some() {
            let pad = rate
                .static_pad("src")
                .ok_or_else(|| CaptureError::Gst("capsfilter has no src pad".into()))?;
            snap_kept_frames(&pad, video.fps, stats.clone());
        }
        chain.attach(0, sink.clone(), stats.clone(), Timing::AsIs);
        let mut session = Self::run(pipeline, sink.clone(), stats.clone(), (w0, h0), timebase)?;
        watch_portal(&portal, &session.stop, sink, stats);
        session.portal = Some(portal);
        session.api = CaptureApi::Portal;
        let ((src_w, src_h), (out_w, out_h)) =
            rx.recv_timeout(PORTAL_FIRST_FRAME).map_err(|_| {
                CaptureError::Target(format!(
                    "no frame from the screen-cast stream within {} s",
                    PORTAL_FIRST_FRAME.as_secs()
                ))
            })?;
        session.output = (out_w, out_h);
        tracing::info!(width = out_w, height = out_h, source_width = src_w, source_height = src_h, fps = video.fps, encoder = %encode.encoder.element, node = stream.node_id, "capture started (screen-cast portal)");
        Ok(session)
    }

    /// Start an already-built pipeline and watch its bus. Used by [`CaptureSession::start`],
    /// the audio session and tests that build their own source. With a `timebase`, the
    /// pipeline uses its clock and base time.
    pub fn run(
        pipeline: gst::Pipeline,
        sink: Arc<dyn PacketSink>,
        stats: Arc<CaptureStats>,
        output: (u32, u32),
        timebase: Option<&TimeBase>,
    ) -> Result<CaptureSession, CaptureError> {
        if let Some(tb) = timebase {
            tb.apply(&pipeline);
        }
        let bus = pipeline
            .bus()
            .ok_or_else(|| CaptureError::Gst("pipeline has no bus".into()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_stats = stats.clone();
        let bus_thread = std::thread::Builder::new()
            .name("klip-capture-bus".into())
            .spawn(move || {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    while !thread_stop.load(Ordering::Relaxed) {
                        let Some(msg) = bus.timed_pop_filtered(
                            gst::ClockTime::from_mseconds(100),
                            &[
                                gst::MessageType::Error,
                                gst::MessageType::Warning,
                                gst::MessageType::Eos,
                            ],
                        ) else {
                            continue;
                        };
                        match msg.view() {
                            gst::MessageView::Error(e) => {
                                thread_stats.errors.fetch_add(1, Ordering::Relaxed);
                                let src =
                                    msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                                sink.on_error(format!("{src}: {}", e.error()));
                            }
                            gst::MessageView::Warning(w) => {
                                thread_stats.warnings.fetch_add(1, Ordering::Relaxed);
                                let src =
                                    msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                                sink.on_warning(format!("{src}: {}", w.error()));
                            }
                            gst::MessageView::Eos(_) => {
                                sink.on_error("capture stream ended".into());
                            }
                            _ => {}
                        }
                    }
                }));
                if r.is_err() {
                    // Capture errors would go unseen from here on: say so, so the
                    // recorder restarts the capture.
                    tracing::error!("the capture bus thread panicked");
                    thread_stats.errors.fetch_add(1, Ordering::Relaxed);
                    sink.on_error("the capture watcher failed".into());
                }
            })
            .map_err(|e| CaptureError::Gst(format!("cannot start bus thread: {e}")))?;
        let session = CaptureSession {
            pipeline,
            stop,
            bus_thread: Some(bus_thread),
            stats,
            output,
            #[cfg(windows)]
            gpu: None,
            portal: None,
            api: CaptureApi::Auto,
        };
        session
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| CaptureError::Gst(format!("could not start pipeline: {e}")))?;
        Ok(session)
    }

    /// Encoded output size.
    pub fn output_size(&self) -> (u32, u32) {
        self.output
    }

    pub fn stats(&self) -> &CaptureStats {
        &self.stats
    }

    /// The capture implementation that runs (`Auto` for pipelines built by tests).
    pub fn api(&self) -> CaptureApi {
        self.api
    }

    /// Frames the source has produced so far (Scout's capture: captured frames; GStreamer
    /// sources: encoded packets). For stall detection.
    pub fn frames_captured(&self) -> u64 {
        #[cfg(windows)]
        if let Some(g) = &self.gpu {
            return g.stats().frames.load(Ordering::Relaxed);
        }
        self.stats.packets.load(Ordering::Relaxed)
    }

    /// The pipeline or the capture reported an error.
    pub fn failed(&self) -> bool {
        self.stats.errors.load(Ordering::Relaxed) > 0
    }

    /// Counters of Scout's own capture, when it runs.
    #[cfg(windows)]
    pub fn gpu_stats(&self) -> Option<Arc<klip_platform::GpuCaptureStats>> {
        self.gpu.as_ref().map(klip_platform::GpuCapture::stats)
    }

    /// The pipeline (for tests that drive an appsrc).
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    /// Stop the pipeline and the bus thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // Stop feeding before the pipeline goes down.
        #[cfg(windows)]
        drop(self.gpu.take());
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Some(t) = self.bus_thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_size_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_size(3840, 2160, 1920, 1080), (1920, 1080));
        assert_eq!(fit_size(1280, 720, 1920, 1080), (1280, 720));
        assert_eq!(fit_size(2560, 1080, 1920, 1080), (1920, 810));
        assert_eq!(fit_size(1080, 1920, 1920, 1080), (606, 1080));
        assert_eq!(fit_size(1281, 721, 1920, 1080), (1280, 720));
        assert_eq!(fit_size(0, 0, 0, 0), (64, 64));
        assert_eq!(fit_size(u32::MAX, u32::MAX, 1920, 1080), (1080, 1080));
    }

    /// Regression: NVENC output carried a 1000 h offset, so audio (on the real timeline)
    /// looked older than every video frame and was evicted from the ring.
    #[test]
    fn encoder_offset_is_undone_then_grid_applied() {
        const HOURS_1000: Nanos = 3_600_000_000_000_000;
        let stats = CaptureStats::default();
        let first_in = 5_000_000_000;
        let origin = InputOrigin(Arc::new(OnceLock::from(first_in)));
        let mut st = SinkState::new(Timing::FrameGrid(60), Some(origin));
        let out: Vec<(Nanos, Nanos)> = (0..4)
            .map(|i| {
                // Encoder output: input time + 1000 h - first input, with 1 ms jitter.
                let t = HOURS_1000 + i * 16_666_667 + if i == 2 { 1_000_000 } else { 0 };
                st.retime(t, t, &stats)
            })
            .collect();
        assert_eq!(
            out,
            [
                (5_000_000_000, 5_000_000_000),
                (5_016_666_667, 5_016_666_667),
                (5_033_333_333, 5_033_333_333),
                (5_050_000_000, 5_050_000_000),
            ]
        );
        // Without an origin, timestamps pass through (audio, appsrc tests).
        let mut plain = SinkState::new(Timing::Shift(-10), None);
        assert_eq!(plain.retime(100, 90, &stats), (90, 80));
    }

    #[test]
    fn caps_to_track_info() {
        klip_probe::init().unwrap();
        let caps = gst::Caps::builder("video/x-h264")
            .field("width", 1280i32)
            .field("height", 720i32)
            .field("colorimetry", "bt709")
            .field("codec_data", gst::Buffer::from_slice([1u8, 0x64, 0, 0x28]))
            .build();
        let info = track_info_from_caps(&caps).unwrap();
        assert_eq!(info.codec, Codec::H264);
        assert_eq!(&info.codec_data[..], &[1, 0x64, 0, 0x28]);
        match info.params {
            TrackParams::Video(v) => {
                assert_eq!((v.width, v.height), (1280, 720));
                assert_eq!(v.colorimetry, Some(Colorimetry::BT709));
            }
            TrackParams::Audio(_) => panic!("video expected"),
        }
        let no_data = gst::Caps::builder("video/x-h264")
            .field("width", 1280i32)
            .field("height", 720i32)
            .build();
        assert!(track_info_from_caps(&no_data).is_err());
        let bad = gst::Caps::builder("video/x-h264")
            .field("width", -5i32)
            .field("height", 720i32)
            .field("codec_data", gst::Buffer::from_slice([1u8]))
            .build();
        assert!(track_info_from_caps(&bad).is_err());
        assert!(track_info_from_caps(&gst::Caps::new_empty_simple("audio/mpeg")).is_err());
    }
}
