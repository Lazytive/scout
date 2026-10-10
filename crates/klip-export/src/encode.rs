//! Writing the output: one GStreamer pipeline with the recorder's encode chain
//! (`klip_capture::build_encode_chain`: the same ranked hardware encoders) fed by an
//! `appsrc` of BGRA frames, and the recorder's AAC chain fed by an `appsrc` of S16
//! samples. Packets are collected in memory and muxed with `klip_mp4` at the end.
//!
//! The `appsrc`s are not blocking: a blocking push would never return once the
//! pipeline has failed with its queue full. Instead the pusher waits for room itself,
//! looking at the bus while it waits.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use klip_capture::audio::{self, SAMPLE_RATE};
use klip_capture::{
    CaptureStats, EncodeConfig, PacketSink, Timing, attach_sink_with, gst_err, make,
};
use klip_media::{NANOS_PER_SEC, Packet, TrackInfo};
use klip_probe::EncoderChoice;

use crate::ExportError;

/// Longest wait for the encoders to flush at the end.
const FINISH: Duration = Duration::from_secs(60);
/// Longest wait for the encoder to take one more frame.
const ROOM: Duration = Duration::from_secs(30);
/// Samples per audio buffer pushed (100 ms).
const AUDIO_CHUNK_FRAMES: usize = 4_800;
/// Frames the video appsrc holds before `push` waits.
const VIDEO_QUEUE_FRAMES: u64 = 4;
/// Bytes the audio appsrc holds before `push` waits.
const AUDIO_QUEUE_BYTES: u64 = 1 << 22;

/// What one pass encodes with.
#[derive(Clone, Debug)]
pub struct EncodeSettings {
    pub encoder: EncoderChoice,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub max_bitrate_kbps: Option<u32>,
    /// AAC bitrate, bit/s (one of `klip_capture::audio::BITRATES`); `None`: no audio.
    pub audio_bps: Option<u32>,
}

/// The encoded result of a pass.
pub struct Encoded {
    pub video: (TrackInfo, Vec<Packet>),
    pub audio: Option<(TrackInfo, Vec<Packet>)>,
    pub encoder: String,
}

#[derive(Default)]
struct Collector {
    info: Mutex<[Option<TrackInfo>; 2]>,
    packets: Mutex<[Vec<Packet>; 2]>,
    errors: Mutex<Vec<String>>,
}

impl Collector {
    fn first_error(&self) -> Option<String> {
        self.errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first()
            .cloned()
    }
}

impl PacketSink for Collector {
    fn on_track_info(&self, track: usize, info: TrackInfo) {
        if let Some(slot) = self
            .info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(track)
        {
            *slot = Some(info);
        }
    }

    fn on_packet(&self, track: usize, packet: Packet) {
        if let Some(v) = self
            .packets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(track)
        {
            v.push(packet);
        }
    }

    fn on_error(&self, message: String) {
        self.errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message);
    }
}

/// A running encode pipeline.
pub struct Encoder {
    pipeline: gst::Pipeline,
    video_src: gst_app::AppSrc,
    audio_src: Option<gst_app::AppSrc>,
    sink: Arc<Collector>,
    settings: EncodeSettings,
    /// The AAC encoder's delay: audio is pushed this much late so the shifted packets
    /// start at 0 (a negative first pts would become the file's origin).
    audio_delay_ns: u64,
    audio_frames: u64,
    /// Frames pushed so far; every one must come back as a packet.
    frames: AtomicU64,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn clock(d: Duration) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

fn bus_error(pipeline: &gst::Pipeline) -> Option<String> {
    let bus = pipeline.bus()?;
    let msg = bus.pop_filtered(&[gst::MessageType::Error])?;
    match msg.view() {
        gst::MessageView::Error(e) => Some(e.error().to_string()),
        _ => None,
    }
}

impl Encoder {
    /// Build and start the pipeline. An encoder that fails to start is an
    /// [`ExportError::Encoder`]; the caller may try the next choice.
    pub fn start(settings: &EncodeSettings) -> Result<Encoder, ExportError> {
        if settings.fps == 0 || settings.width < 2 || settings.height < 2 {
            return Err(ExportError::Plan("bad output size or rate".into()));
        }
        let pipeline = gst::Pipeline::with_name("klip-export-encode");
        let r = Self::build(&pipeline, settings);
        if r.is_err() {
            // Elements only free their resources (an NVENC session, a Media Foundation
            // transform) from NULL.
            let _ = pipeline.set_state(gst::State::Null);
        }
        r
    }

    fn build(pipeline: &gst::Pipeline, settings: &EncodeSettings) -> Result<Encoder, ExportError> {
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "BGRA")
            .field("width", i32::try_from(settings.width).unwrap_or(i32::MAX))
            .field("height", i32::try_from(settings.height).unwrap_or(i32::MAX))
            .field(
                "framerate",
                gst::Fraction::new(i32::try_from(settings.fps).unwrap_or(60), 1),
            )
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        let frame_bytes = u64::from(settings.width)
            .saturating_mul(u64::from(settings.height))
            .saturating_mul(4);
        let video_src = gst_app::AppSrc::builder()
            .caps(&caps)
            .format(gst::Format::Time)
            .block(false)
            .max_bytes(frame_bytes.saturating_mul(VIDEO_QUEUE_FRAMES))
            .build();
        let upload = make(if cfg!(windows) {
            "d3d11upload"
        } else {
            "identity"
        })?;
        pipeline
            .add_many([video_src.upcast_ref(), &upload])
            .map_err(gst_err)?;
        video_src.link(&upload).map_err(gst_err)?;
        let cfg = EncodeConfig {
            encoder: settings.encoder.clone(),
            bitrate_kbps: settings.bitrate_kbps,
            keyframe_interval: settings.fps,
            max_bitrate_kbps: settings.max_bitrate_kbps,
            leaky_queue: false,
        };
        let chain = klip_capture::build_encode_chain(
            pipeline,
            &cfg,
            settings.width,
            settings.height,
            settings.fps,
        )?;
        upload.link(&chain.input).map_err(gst_err)?;
        let sink = Arc::new(Collector::default());
        let stats = Arc::new(CaptureStats::default());
        let dyn_sink: Arc<dyn PacketSink> = sink.clone();
        chain.attach(0, dyn_sink.clone(), stats.clone(), Timing::AsIs);

        let mut audio_delay_ns = 0;
        let audio_src = match settings.audio_bps {
            Some(bps) => {
                let (input, appsink) = audio::build_aac_chain(pipeline, bps)?;
                // Audio is pushed after the video; a sink that waits for its first
                // buffer before the pipeline may play would deadlock the frame pushes.
                appsink.set_property("async", false);
                let acaps = gst::Caps::builder("audio/x-raw")
                    .field("format", "S16LE")
                    .field("layout", "interleaved")
                    .field("rate", i32::try_from(SAMPLE_RATE).unwrap_or(48_000))
                    .field("channels", i32::try_from(audio::CHANNELS).unwrap_or(2))
                    .build();
                let src = gst_app::AppSrc::builder()
                    .caps(&acaps)
                    .format(gst::Format::Time)
                    .block(false)
                    .max_bytes(AUDIO_QUEUE_BYTES)
                    .build();
                pipeline.add(&src).map_err(gst_err)?;
                src.link(&input).map_err(gst_err)?;
                let delay = audio::aac_encoder().map_or(0, audio::aac_encoder_delay);
                audio_delay_ns = u64::try_from(delay).unwrap_or(0);
                attach_sink_with(
                    &appsink,
                    1,
                    dyn_sink,
                    stats.clone(),
                    Timing::Shift(delay.saturating_neg()),
                );
                Some(src)
            }
            None => None,
        };
        if pipeline.set_state(gst::State::Playing).is_err() {
            let why = bus_error(pipeline).unwrap_or_else(|| "it did not start".into());
            return Err(ExportError::Encoder(format!(
                "{}: {why}",
                settings.encoder.element
            )));
        }
        Ok(Encoder {
            pipeline: pipeline.clone(),
            video_src,
            audio_src,
            sink,
            settings: settings.clone(),
            audio_delay_ns,
            audio_frames: 0,
            frames: AtomicU64::new(0),
        })
    }

    fn frame_ns(&self, index: u64) -> u64 {
        index
            .saturating_mul(NANOS_PER_SEC as u64)
            .checked_div(u64::from(self.settings.fps))
            .unwrap_or(0)
    }

    fn check(&self) -> Result<(), ExportError> {
        if let Some(e) = bus_error(&self.pipeline) {
            return Err(ExportError::Encoder(format!(
                "{}: {e}",
                self.settings.encoder.element
            )));
        }
        if let Some(e) = self.sink.first_error() {
            return Err(ExportError::Encoder(e));
        }
        Ok(())
    }

    /// Push `buffer` into `src` once it has room, watching the bus meanwhile.
    fn push(
        &self,
        src: &gst_app::AppSrc,
        buffer: gst::Buffer,
        max_bytes: u64,
        what: &str,
    ) -> Result<(), ExportError> {
        let deadline = Instant::now().checked_add(ROOM);
        while src.current_level_bytes() >= max_bytes {
            self.check()?;
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(ExportError::Encoder(format!(
                    "{what}: the encoder stopped taking data"
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if src.push_buffer(buffer).is_err() {
            self.check()?;
            return Err(ExportError::Encoder(format!(
                "{what}: the encoder refused data"
            )));
        }
        self.check()
    }

    /// Push output frame `index` (`buffer` must be BGRA at the output size).
    pub fn push_frame(&self, mut buffer: gst::Buffer, index: u64) -> Result<(), ExportError> {
        let pts = self.frame_ns(index);
        let dur = self.frame_ns(index.saturating_add(1)).saturating_sub(pts);
        {
            let b = buffer
                .get_mut()
                .ok_or_else(|| ExportError::Gst("frame buffer is shared".into()))?;
            b.set_pts(gst::ClockTime::from_nseconds(pts));
            b.set_dts(gst::ClockTime::from_nseconds(pts));
            b.set_duration(gst::ClockTime::from_nseconds(dur));
        }
        let max = u64::from(self.settings.width)
            .saturating_mul(u64::from(self.settings.height))
            .saturating_mul(4)
            .saturating_mul(VIDEO_QUEUE_FRAMES);
        self.push(&self.video_src, buffer, max, "video")?;
        self.frames.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn audio_ns(&self, frames: u64) -> u64 {
        frames
            .saturating_mul(NANOS_PER_SEC as u64)
            .checked_div(u64::from(SAMPLE_RATE))
            .unwrap_or(0)
            .saturating_add(self.audio_delay_ns)
    }

    /// Push interleaved stereo S16 samples after everything pushed so far.
    pub fn push_audio(&mut self, samples: &[i16]) -> Result<(), ExportError> {
        let Some(src) = self.audio_src.clone() else {
            return Ok(());
        };
        let per_frame = audio::CHANNELS as usize;
        for chunk in samples.chunks(AUDIO_CHUNK_FRAMES.saturating_mul(per_frame)) {
            let frames = chunk.len().checked_div(per_frame).unwrap_or(0) as u64;
            let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut buffer = gst::Buffer::from_mut_slice(bytes);
            let pts = self.audio_ns(self.audio_frames);
            let end = self.audio_ns(self.audio_frames.saturating_add(frames));
            if let Some(b) = buffer.get_mut() {
                b.set_pts(gst::ClockTime::from_nseconds(pts));
                b.set_duration(gst::ClockTime::from_nseconds(end.saturating_sub(pts)));
            }
            self.push(&src, buffer, AUDIO_QUEUE_BYTES, "audio")?;
            self.audio_frames = self.audio_frames.saturating_add(frames);
        }
        Ok(())
    }

    /// End both streams, wait for the encoders to flush, and hand back the packets.
    pub fn finish(self) -> Result<Encoded, ExportError> {
        let _ = self.video_src.end_of_stream();
        if let Some(a) = &self.audio_src {
            let _ = a.end_of_stream();
        }
        let bus = self
            .pipeline
            .bus()
            .ok_or_else(|| ExportError::Gst("encode pipeline without a bus".into()))?;
        match bus.timed_pop_filtered(
            Some(clock(FINISH)),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            Some(msg) => {
                if let gst::MessageView::Error(e) = msg.view() {
                    return Err(ExportError::Encoder(e.error().to_string()));
                }
            }
            None => return Err(ExportError::Encoder("the encoder did not finish".into())),
        }
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Some(e) = self.sink.first_error() {
            return Err(ExportError::Encoder(e));
        }
        let mut info = self
            .sink
            .info
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut packets = self
            .sink
            .packets
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let video_info = info
            .get_mut(0)
            .and_then(Option::take)
            .ok_or_else(|| ExportError::Encoder("no video was encoded".into()))?;
        let video_packets = packets.get_mut(0).map(std::mem::take).unwrap_or_default();
        // The queue in front of the encoder blocks instead of dropping (`leaky_queue:
        // false`), so every pushed frame must be here.
        let pushed = self.frames.load(Ordering::Relaxed);
        if video_packets.is_empty() || video_packets.len() as u64 != pushed {
            return Err(ExportError::Encoder(format!(
                "the encoder returned {} of {pushed} frames",
                video_packets.len()
            )));
        }
        let audio = match (
            info.get_mut(1).and_then(Option::take),
            packets.get_mut(1).map(std::mem::take),
        ) {
            (Some(i), Some(p)) if !p.is_empty() => Some((i, p)),
            _ => None,
        };
        Ok(Encoded {
            video: (video_info, video_packets),
            audio,
            encoder: self.settings.encoder.element.clone(),
        })
    }
}

/// Run `f` on a GStreamer thread-safe path: panics become errors.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, ExportError>) -> Result<T, ExportError> {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(ExportError::Gst("internal error".into())))
}
