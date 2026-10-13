//! Writing the output: one GStreamer pipeline with the recorder's encode chain
//! (`klip_capture::build_encode_chain`: the same ranked hardware encoders) fed by an
//! `appsrc` of BGRA frames, and the recorder's AAC chain fed by an `appsrc` of S16
//! samples. Packet bytes go to a spool file next to the output as they come (only
//! their metadata stays in memory, IMPROVEMENTS G1); `klip_mp4::write_mp4_spooled`
//! muxes them at the end.
//!
//! The `appsrc`s are not blocking: a blocking push would never return once the
//! pipeline has failed with its queue full. Instead the pusher waits for room itself,
//! looking at the bus while it waits.

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
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
use klip_mp4::Sample;
use klip_probe::EncoderChoice;

use crate::ExportError;
use crate::files::OwnedFile;

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
/// The audio track's index in the collector (video is 0).
const AUDIO: usize = 1;

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

/// The encoded result of a pass: sample metadata per track, bytes in `spool`.
pub struct Encoded {
    pub video: (TrackInfo, Vec<Sample>),
    pub audio: Option<(TrackInfo, Vec<Sample>)>,
    pub encoder: String,
    /// Removed when this is dropped.
    pub spool: OwnedFile,
}

/// Where packet bytes go.
#[derive(Default)]
struct Spool {
    out: Option<BufWriter<File>>,
    offset: u64,
}

#[derive(Default)]
struct Collector {
    info: Mutex<[Option<TrackInfo>; 2]>,
    samples: Mutex<[Vec<Sample>; 2]>,
    spool: Mutex<Spool>,
    errors: Mutex<Vec<String>>,
}

impl Collector {
    /// Flush and close the spool (a file still open can't be removed on Windows).
    fn close(&self) -> std::io::Result<()> {
        let out = self
            .spool
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .out
            .take();
        match out {
            Some(mut w) => w.flush(),
            None => Ok(()),
        }
    }

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
        // Audio shifted back by the AAC encoder's delay starts with its priming frames
        // before 0; drop them, as a clip cut from the recorder's buffer does. (Kept, the
        // earliest one would become the file's origin and move everything by the delay.)
        // By their end: the first real frame may start a rounding nanosecond before 0.
        if track == AUDIO && packet.pts < 0 && packet.dts_end() <= 0 {
            return;
        }
        let mut spool = self.spool.lock().unwrap_or_else(PoisonError::into_inner);
        let offset = spool.offset;
        let Some(out) = spool.out.as_mut() else {
            return;
        };
        let mut sample = match Sample::of(&packet) {
            Ok(s) => s,
            Err(e) => {
                drop(spool);
                self.on_error(e.to_string());
                return;
            }
        };
        sample.offset = offset;
        if let Err(e) = out.write_all(&packet.data) {
            spool.out = None;
            drop(spool);
            self.on_error(format!("the spool could not be written: {e}"));
            return;
        }
        spool.offset = offset.saturating_add(u64::from(sample.size));
        drop(spool);
        if let Some(v) = self
            .samples
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(track)
        {
            v.push(sample);
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
    /// The packet bytes; handed to [`Encoded`] by [`Encoder::finish`].
    spool: Option<OwnedFile>,
    settings: EncodeSettings,
    audio_frames: u64,
    /// Frames pushed so far; every one must come back as a packet.
    frames: AtomicU64,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        // Before `spool` removes the file.
        let _ = self.sink.close();
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
    /// Build and start the pipeline, spooling packet bytes to `spool` (created here,
    /// must not exist; removed again when the encoder fails or its result is dropped).
    /// An encoder that fails to start is an [`ExportError::Encoder`]; the caller may
    /// try the next choice.
    pub fn start(settings: &EncodeSettings, spool: &Path) -> Result<Encoder, ExportError> {
        if settings.fps == 0 || settings.width < 2 || settings.height < 2 {
            return Err(ExportError::Plan("bad output size or rate".into()));
        }
        let (owned, file) = OwnedFile::create(spool)?;
        let sink = Arc::new(Collector {
            spool: Mutex::new(Spool {
                out: Some(BufWriter::with_capacity(1 << 20, file)),
                offset: 0,
            }),
            ..Collector::default()
        });
        let pipeline = gst::Pipeline::with_name("klip-export-encode");
        let r = Self::build(&pipeline, settings, sink.clone());
        if r.is_err() {
            // Elements only free their resources (an NVENC session, a Media Foundation
            // transform) from NULL.
            let _ = pipeline.set_state(gst::State::Null);
            // Closed before `owned` removes it, so the next choice can create it again.
            let _ = sink.close();
        }
        r.map(|mut e| {
            e.spool = Some(owned);
            e
        })
    }

    fn build(
        pipeline: &gst::Pipeline,
        settings: &EncodeSettings,
        sink: Arc<Collector>,
    ) -> Result<Encoder, ExportError> {
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
        let stats = Arc::new(CaptureStats::default());
        let dyn_sink: Arc<dyn PacketSink> = sink.clone();
        chain.attach(0, dyn_sink.clone(), stats.clone(), Timing::AsIs);

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
                // The encoder's priming lands before 0 and is dropped by the collector.
                let delay = audio::aac_encoder().map_or(0, audio::aac_encoder_delay);
                attach_sink_with(
                    &appsink,
                    AUDIO,
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
            spool: None,
            settings: settings.clone(),
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
    pub fn finish(mut self) -> Result<Encoded, ExportError> {
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
        self.sink.close()?;
        let mut info = self
            .sink
            .info
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut samples = self
            .sink
            .samples
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let video_info = info
            .get_mut(0)
            .and_then(Option::take)
            .ok_or_else(|| ExportError::Encoder("no video was encoded".into()))?;
        let video_samples = samples.get_mut(0).map(std::mem::take).unwrap_or_default();
        // The queue in front of the encoder blocks instead of dropping (`leaky_queue:
        // false`), so every pushed frame must be here.
        let pushed = self.frames.load(Ordering::Relaxed);
        if video_samples.is_empty() || video_samples.len() as u64 != pushed {
            return Err(ExportError::Encoder(format!(
                "the encoder returned {} of {pushed} frames",
                video_samples.len()
            )));
        }
        let audio = match (
            info.get_mut(1).and_then(Option::take),
            samples.get_mut(1).map(std::mem::take),
        ) {
            (Some(i), Some(p)) if !p.is_empty() => Some((i, p)),
            _ => None,
        };
        drop(info);
        drop(samples);
        let spool = self
            .spool
            .take()
            .ok_or_else(|| ExportError::Encoder("the spool is gone".into()))?;
        Ok(Encoded {
            video: (video_info, video_samples),
            audio,
            encoder: self.settings.encoder.element.clone(),
            spool,
        })
    }
}

/// Run `f` on a GStreamer thread-safe path: panics become errors.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, ExportError>) -> Result<T, ExportError> {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(ExportError::Gst("internal error".into())))
}
