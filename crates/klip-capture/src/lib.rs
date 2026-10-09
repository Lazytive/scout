//! Capture + hardware-encode pipelines that deliver **encoded packets** to a sink.
//!
//! Windows video path (ADR 0002):
//! `d3d11screencapturesrc (WGC) → d3d11convert → NV12 at a fixed size → queue → encoder →
//! parser → appsink`. The output size is fixed when the session starts, so window resizes
//! are scaled/letterboxed instead of changing the encoder's caps mid-stream.
//!
//! The encode half ([`build_encode_chain`]) is public so tests and benchmarks can feed it
//! from an `appsrc` with known frames.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod props;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use klip_media::{
    Codec, Colorimetry, MAX_PACKET_BYTES, Nanos, Packet, TrackInfo, TrackParams, VideoParams,
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
}

/// Live counters of a session.
#[derive(Debug, Default)]
pub struct CaptureStats {
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    pub keyframes: AtomicU64,
    pub errors: AtomicU64,
    pub callback_panics: AtomicU64,
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

/// Apply rate control and GOP settings for an encoder family. Unknown properties are
/// skipped with a log line; the required ones (bitrate, keyframe interval) error out.
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
            props::set_str(el, "bframes", "0");
            props::set_str(el, "low-latency", "true");
        }
        EncoderFamily::Amf | EncoderFamily::Qsv => {
            // Property names verified on AMD/Intel hardware in M3.
            req("bitrate", &kbps)?;
            req("gop-size", &gop)?;
            props::set_str(el, "max-bitrate", &max_kbps);
            props::set_str(el, "b-frames", "0");
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

/// Build `d3d11convert → NV12 (w x h) → [download/upload] → queue → encoder → parser →
/// appsink` inside `bin`. Input must be `video/x-raw(memory:D3D11Memory)`. Returns the
/// first element (link your source to it) and the appsink.
pub fn build_encode_chain(
    bin: &gst::Pipeline,
    cfg: &EncodeConfig,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(gst::Element, gst_app::AppSink), CaptureError> {
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
    let software_mf = family == EncoderFamily::MediaFoundation
        && !cfg.encoder.description.contains("NVIDIA")
        && !cfg.encoder.description.contains("AMD")
        && !cfg.encoder.description.contains("Intel");
    match family {
        EncoderFamily::NvencCuda => {
            chain.push(make("d3d11download")?);
            chain.push(make("cudaupload")?);
        }
        EncoderFamily::VaApi | EncoderFamily::OpenH264 => chain.push(make("d3d11download")?),
        EncoderFamily::MediaFoundation if software_mf => chain.push(make("d3d11download")?),
        _ => {}
    }
    let queue = make("queue")?;
    props::set_str(&queue, "max-size-buffers", "8");
    props::set_str(&queue, "max-size-bytes", "0");
    props::set_str(&queue, "max-size-time", "0");
    props::set_str(&queue, "leaky", "downstream");
    chain.push(queue);
    let encoder = make(&cfg.encoder.element)?;
    if klip_probe::allowed(&cfg.encoder.element).is_none() {
        return Err(CaptureError::Config(format!(
            "{} is not an allowed encoder",
            cfg.encoder.element
        )));
    }
    configure_encoder(&encoder, cfg)?;
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
    Ok((convert, appsink))
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

/// Describe an encoded video stream from its parser caps.
pub fn track_info_from_caps(caps: &gst::CapsRef) -> Result<TrackInfo, String> {
    let s = caps.structure(0).ok_or("empty caps")?;
    let codec = match s.name().as_str() {
        "video/x-h264" => Codec::H264,
        "video/x-h265" => Codec::H265,
        "video/x-av1" => Codec::Av1,
        other => return Err(format!("unsupported caps {other}")),
    };
    let codec_data = s
        .get::<gst::Buffer>("codec_data")
        .map_err(|_| "caps have no codec_data".to_string())?;
    let map = codec_data.map_readable().map_err(|e| e.to_string())?;
    let width = s.get::<i32>("width").map_err(|e| e.to_string())?;
    let height = s.get::<i32>("height").map_err(|e| e.to_string())?;
    let info = TrackInfo {
        codec,
        codec_data: Bytes::copy_from_slice(map.as_slice()),
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

struct SinkState {
    caps: Option<gst::Caps>,
}

fn handle_sample(
    appsink: &gst_app::AppSink,
    track: usize,
    sink: &dyn PacketSink,
    stats: &CaptureStats,
    state: &Mutex<SinkState>,
) -> Result<(), String> {
    let sample = appsink.pull_sample().map_err(|e| e.to_string())?;
    if let Some(caps) = sample.caps() {
        let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.caps.as_ref().map(|c| c.as_ref()) != Some(caps) {
            let info = track_info_from_caps(caps)?;
            st.caps = Some(caps.to_owned());
            sink.on_track_info(track, info);
        }
    }
    let buffer = sample.buffer().ok_or("sample without buffer")?;
    if buffer.size() > MAX_PACKET_BYTES {
        return Err(format!("encoded frame too large ({} bytes)", buffer.size()));
    }
    let map = buffer.map_readable().map_err(|e| e.to_string())?;
    let pts = clock_ns(buffer.pts()).ok_or("buffer without timestamp")?;
    let dts = clock_ns(buffer.dts()).unwrap_or(pts);
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

/// Route an appsink's samples to `sink` as packets of `track`. Panics inside the callback
/// are caught and counted; the stream keeps flowing.
pub fn attach_sink(
    appsink: &gst_app::AppSink,
    track: usize,
    sink: Arc<dyn PacketSink>,
    stats: Arc<CaptureStats>,
) {
    let state = Mutex::new(SinkState { caps: None });
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
    /// Build and start a screen/window capture session (Windows).
    pub fn start(
        video: &VideoConfig,
        encode: &EncodeConfig,
        sink: Arc<dyn PacketSink>,
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
        let rate = capsfilter(&format!(
            "video/x-raw(memory:D3D11Memory),framerate={}/1",
            video.fps
        ))?;
        pipeline.add_many([&src, &rate]).map_err(gst_err)?;
        src.link(&rate).map_err(gst_err)?;
        let (first, appsink) = build_encode_chain(&pipeline, encode, out_w, out_h, video.fps)?;
        rate.link(&first).map_err(gst_err)?;

        let stats = Arc::new(CaptureStats::default());
        attach_sink(&appsink, 0, sink.clone(), stats.clone());
        let session = Self::run(pipeline, sink, stats, (out_w, out_h))?;
        tracing::info!(width = out_w, height = out_h, fps = video.fps, encoder = %encode.encoder.element, "capture started");
        Ok(session)
    }

    /// Start an already-built pipeline and watch its bus. Used by [`CaptureSession::start`]
    /// and by tests that build their own source.
    pub fn run(
        pipeline: gst::Pipeline,
        sink: Arc<dyn PacketSink>,
        stats: Arc<CaptureStats>,
        output: (u32, u32),
    ) -> Result<CaptureSession, CaptureError> {
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
                                tracing::warn!(warning = %w.error(), "pipeline warning");
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
