//! Capture + hardware-encode pipelines that deliver **encoded packets** to a sink.
//!
//! Windows video path (ADR 0002):
//! `d3d11screencapturesrc (WGC) → d3d11convert → NV12 at a fixed size → queue → encoder →
//! parser → appsink`. The output size is fixed when the session starts, so window resizes
//! are scaled/letterboxed instead of changing the encoder's caps mid-stream.
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
mod props;

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

fn gst_err(e: impl std::fmt::Display) -> CaptureError {
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
}

/// Video capture settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoConfig {
    pub target: VideoTarget,
    pub fps: u32,
    /// Output is scaled down (never up) to fit in this box, keeping aspect ratio.
    pub max_width: u32,
    pub max_height: u32,
    pub show_cursor: bool,
}

/// Encoder settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeConfig {
    pub encoder: EncoderChoice,
    pub bitrate_kbps: u32,
    /// Keyframe interval in frames (Klip uses 1 s).
    pub keyframe_interval: u32,
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

fn make(name: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| CaptureError::MissingElement(name.to_string()))
}

fn capsfilter(caps: &str) -> Result<gst::Element, CaptureError> {
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
        .bitrate_kbps
        .saturating_mul(3)
        .checked_div(2)
        .unwrap_or(cfg.bitrate_kbps)
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

/// Build `d3d11convert → NV12 (w x h) → [download/upload] → queue → encoder → parser →
/// appsink` inside `bin`. Input must be `video/x-raw(memory:D3D11Memory)`.
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
    let family = cfg.encoder.family;
    let convert = make("d3d11convert")?;
    let mut chain = vec![convert.clone()];
    let nv12 = format!(
        "video/x-raw(memory:D3D11Memory),format=NV12,width={width},height={height},pixel-aspect-ratio=1/1"
    );
    chain.push(capsfilter(&nv12)?);
    match family {
        EncoderFamily::NvencCuda => {
            chain.push(make("d3d11download")?);
            chain.push(make("cudaupload")?);
        }
        // D3D11 → D3D12 on the GPU (shared texture), no download.
        EncoderFamily::D3d12 => chain.push(make("d3d12upload")?),
        _ if cfg.encoder.needs_system_memory() => chain.push(make("d3d11download")?),
        _ => {}
    }
    let queue = make("queue")?;
    props::set_str(&queue, "max-size-buffers", "8");
    props::set_str(&queue, "max-size-bytes", "0");
    props::set_str(&queue, "max-size-time", "0");
    props::set_str(&queue, "leaky", "downstream");
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
    chain.push(encoder);
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
}

impl std::fmt::Debug for CaptureSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSession")
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

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
fn restamp_on_arrival(src: &gst::Element, tb: &TimeBase) -> Result<(), CaptureError> {
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
            let now = tb.now().max(last.saturating_add(1));
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
    }
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
        let (src_w, src_h, target) = source_size(video.target)?;
        let (out_w, out_h) = fit_size(src_w, src_h, video.max_width, video.max_height);

        let pipeline = gst::Pipeline::with_name("klip-capture");
        let src = make("d3d11screencapturesrc")?;
        props::require(&src, "capture-api", "wgc").map_err(CaptureError::Config)?;
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
        }
        if let Some(tb) = timebase {
            restamp_on_arrival(&src, tb)?;
        }
        let source_rate = video
            .fps
            .saturating_mul(SOURCE_RATE_FACTOR)
            .clamp(SOURCE_RATE_MIN, SOURCE_RATE_MAX);
        let poll = capsfilter(&format!(
            "video/x-raw(memory:D3D11Memory),framerate={source_rate}/1"
        ))?;
        let videorate = make("videorate")?;
        // Never invent frames; keep the one nearest each output slot.
        props::set_str(&videorate, "drop-only", "true");
        let rate = capsfilter(&format!(
            "video/x-raw(memory:D3D11Memory),framerate={}/1",
            video.fps
        ))?;
        pipeline
            .add_many([&src, &poll, &videorate, &rate])
            .map_err(gst_err)?;
        gst::Element::link_many([&src, &poll, &videorate, &rate]).map_err(gst_err)?;
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
        let session = Self::run(pipeline, sink, stats, (out_w, out_h), timebase)?;
        tracing::info!(width = out_w, height = out_h, fps = video.fps, encoder = %encode.encoder.element, "capture started");
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
                let _ = catch_unwind(AssertUnwindSafe(|| {
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
            })
            .map_err(|e| CaptureError::Gst(format!("cannot start bus thread: {e}")))?;
        let session = CaptureSession {
            pipeline,
            stop,
            bus_thread: Some(bus_thread),
            stats,
            output,
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

    /// The pipeline (for tests that drive an appsrc).
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    /// Stop the pipeline and the bus thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
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
