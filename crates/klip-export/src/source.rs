//! Reading a clip: one GStreamer pipeline (`filesrc → decodebin`), seeked once per
//! piece of the output with the piece's rate. Video comes out cropped and scaled to the
//! asked size as BGRA (a picture: at its own size, at most [`MAX_PICTURE`]); every audio
//! track comes out as 48 kHz stereo S16 with its tempo already changed by `scaletempo`
//! (pitch kept). A source opened without video (music, a sound effect) leaves the
//! video stream undecoded. Everything is pulled by the
//! export thread; nothing here runs on a timer, so the pipeline goes as fast as the
//! decoder does.
//!
//! One `decodebin` for the whole file (not one per stream): its shared queue lets a
//! starved stream grow while another one is blocked at its sink, which is what keeps
//! preroll from deadlocking when the video sink holds its few frames.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use klip_capture::{capsfilter, gst_err, make, props};
use klip_edit::Rect;

use crate::ExportError;

/// Decoded audio format.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// Interleaved samples per millisecond.
pub const SAMPLES_PER_MS: usize = (SAMPLE_RATE / 1000 * CHANNELS) as usize;

/// How long a state change or seek may take.
const STEP: Duration = Duration::from_secs(20);
/// How long to wait for the next decoded frame.
const PULL: Duration = Duration::from_secs(20);
/// Audio tracks read at most (a hostile file could declare hundreds).
const MAX_AUDIO: usize = 8;
/// Each segment is played this much past its end so `scaletempo` (which keeps up to
/// ~64 ms of input queued) delivers the segment's last audio; the surplus is cut.
const TAIL_MS: u64 = 100;
/// Longest leading audio gap filled with silence (a track that starts late).
const MAX_LEAD_MS: u64 = 2_000;
/// Largest side of a picture decoded at its own size (bigger ones are scaled down).
pub const MAX_PICTURE: u32 = 4096;

/// How the source is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceConfig {
    /// The clip's size as probed.
    pub src_width: u32,
    pub src_height: u32,
    /// The kept part of the frame, in source pixels (empty: all of it).
    pub crop: Rect,
    /// The size frames come out at (0: the picture's own size, at most
    /// [`MAX_PICTURE`] a side).
    pub out_width: u32,
    pub out_height: u32,
    /// `false`: audio only (the video stream is not decoded, and not required).
    pub want_video: bool,
}

/// One decoded frame at the output size.
#[derive(Clone)]
pub struct Frame {
    /// Source presentation time, ms (floor).
    pub pts_ms: u64,
    pub buffer: gst::Buffer,
    pub info: gst_video::VideoInfo,
}

struct AudioTap {
    /// The pad's stream id: sorts the tracks into file order.
    stream_id: String,
    sink: gst_app::AppSink,
}

struct Shared {
    cfg: SourceConfig,
    video: Mutex<Option<gst_app::AppSink>>,
    audio: Mutex<Vec<AudioTap>>,
    error: Mutex<Option<String>>,
    no_more_pads: AtomicBool,
}

impl Shared {
    fn fail(&self, what: &str) {
        let mut e = self.error.lock().unwrap_or_else(PoisonError::into_inner);
        if e.is_none() {
            *e = Some(what.to_string());
        }
    }
}

/// The clip, open and prerolled.
pub struct Source {
    pipeline: gst::Pipeline,
    shared: Arc<Shared>,
    /// The current segment: source start (ns) and rate, for the audio lead-in.
    segment: Mutex<(u64, f64)>,
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Build the video branch behind a decoded raw video pad and expose its appsink.
fn video_branch(pipeline: &gst::Pipeline, pad: &gst::Pad, shared: &Shared) -> Result<(), String> {
    let caps = pad.current_caps().ok_or("video pad without caps")?;
    let s = caps.structure(0).ok_or("empty video caps")?;
    let dec_w = s
        .get::<i32>("width")
        .ok()
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let dec_h = s
        .get::<i32>("height")
        .ok()
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    if dec_w < 2 || dec_h < 2 {
        return Err("the decoded picture is too small".into());
    }
    // The crop was planned on the probed size; map it onto what was decoded.
    let mut cfg = shared.cfg;
    if cfg.crop.width == 0 || cfg.crop.height == 0 {
        cfg.crop = Rect {
            x: 0,
            y: 0,
            width: dec_w,
            height: dec_h,
        };
        cfg.src_width = dec_w;
        cfg.src_height = dec_h;
    }
    let scale = |v: u32, from: u32, to: u32| -> u32 {
        if from == 0 || from == to {
            return v;
        }
        u32::try_from(
            u64::from(v)
                .saturating_mul(u64::from(to))
                .checked_div(u64::from(from))
                .unwrap_or(0),
        )
        .unwrap_or(0)
    };
    let x = scale(cfg.crop.x, cfg.src_width, dec_w).min(dec_w.saturating_sub(2));
    let y = scale(cfg.crop.y, cfg.src_height, dec_h).min(dec_h.saturating_sub(2));
    let (max_w, max_h) = (dec_w.saturating_sub(x), dec_h.saturating_sub(y));
    if max_w < 2 || max_h < 2 {
        return Err("the crop leaves no picture".into());
    }
    let w = scale(cfg.crop.width, cfg.src_width, dec_w)
        .min(max_w)
        .max(2)
        & !1;
    let h = scale(cfg.crop.height, cfg.src_height, dec_h)
        .min(max_h)
        .max(2)
        & !1;
    let crop = make("videocrop").map_err(|e| e.to_string())?;
    for (name, v) in [
        ("left", x),
        ("top", y),
        ("right", dec_w.saturating_sub(x).saturating_sub(w)),
        ("bottom", dec_h.saturating_sub(y).saturating_sub(h)),
    ] {
        props::require(&crop, name, &v.to_string())?;
    }
    let scaler = make("videoconvertscale").map_err(|e| e.to_string())?;
    props::set_str(&scaler, "n-threads", "0");
    // Fill the (even-rounded) output exactly instead of letterboxing a 1 px difference.
    props::set_str(&scaler, "add-borders", "false");
    let side = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    let out_caps = if cfg.out_width == 0 || cfg.out_height == 0 {
        // The picture's own size; the scaler keeps the aspect when it has to shrink.
        gst::Caps::builder("video/x-raw")
            .field("format", "BGRA")
            .field("width", gst::IntRange::new(2, side(MAX_PICTURE)))
            .field("height", gst::IntRange::new(2, side(MAX_PICTURE)))
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build()
    } else {
        gst::Caps::builder("video/x-raw")
            .field("format", "BGRA")
            .field("width", side(cfg.out_width))
            .field("height", side(cfg.out_height))
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build()
    };
    let sink = gst_app::AppSink::builder()
        .caps(&out_caps)
        .sync(false)
        .max_buffers(4)
        .drop(false)
        .enable_last_sample(false)
        .build();
    let elements = [crop.clone(), scaler, sink.clone().upcast()];
    pipeline.add_many(&elements).map_err(|e| e.to_string())?;
    gst::Element::link_many(&elements).map_err(|e| e.to_string())?;
    let sinkpad = crop.static_pad("sink").ok_or("videocrop has no sink pad")?;
    pad.link(&sinkpad)
        .map_err(|e| format!("link video: {e:?}"))?;
    for e in &elements {
        e.sync_state_with_parent().map_err(|e| e.to_string())?;
    }
    *shared.video.lock().unwrap_or_else(PoisonError::into_inner) = Some(sink);
    Ok(())
}

/// Build the audio branch behind a decoded raw audio pad.
fn audio_branch(pipeline: &gst::Pipeline, pad: &gst::Pad, shared: &Shared) -> Result<(), String> {
    if shared
        .audio
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len()
        >= MAX_AUDIO
    {
        return Ok(());
    }
    let convert = make("audioconvert").map_err(|e| e.to_string())?;
    let resample = make("audioresample").map_err(|e| e.to_string())?;
    let tempo = make("scaletempo").map_err(|e| e.to_string())?;
    let convert2 = make("audioconvert").map_err(|e| e.to_string())?;
    let caps = capsfilter(&format!(
        "audio/x-raw,format=S16LE,layout=interleaved,rate={SAMPLE_RATE},channels={CHANNELS}"
    ))
    .map_err(|e| e.to_string())?;
    let sink = gst_app::AppSink::builder()
        .sync(false)
        .max_buffers(0)
        .enable_last_sample(false)
        .build();
    let elements = [
        convert.clone(),
        resample,
        tempo,
        convert2,
        caps,
        sink.clone().upcast(),
    ];
    pipeline.add_many(&elements).map_err(|e| e.to_string())?;
    gst::Element::link_many(&elements).map_err(|e| e.to_string())?;
    let sinkpad = convert
        .static_pad("sink")
        .ok_or("audioconvert has no sink pad")?;
    pad.link(&sinkpad)
        .map_err(|e| format!("link audio: {e:?}"))?;
    for e in &elements {
        e.sync_state_with_parent().map_err(|e| e.to_string())?;
    }
    let stream_id = pad
        .stream_id()
        .map(|s| s.to_string())
        .unwrap_or_else(|| pad.name().to_string());
    shared
        .audio
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(AudioTap { stream_id, sink });
    Ok(())
}

/// A stream nobody reads (undecoded video of an audio-only source): into a fakesink,
/// so decodebin's queue never waits on it.
fn discard(pipeline: &gst::Pipeline, pad: &gst::Pad) -> Result<(), String> {
    let sink = make("fakesink").map_err(|e| e.to_string())?;
    props::set_str(&sink, "sync", "false");
    props::set_str(&sink, "async", "false");
    pipeline.add(&sink).map_err(|e| e.to_string())?;
    let sinkpad = sink.static_pad("sink").ok_or("fakesink has no sink pad")?;
    pad.link(&sinkpad)
        .map_err(|e| format!("link discard: {e:?}"))?;
    sink.sync_state_with_parent().map_err(|e| e.to_string())?;
    Ok(())
}

fn on_pad(pipeline: &gst::Pipeline, pad: &gst::Pad, shared: &Shared) -> Result<(), String> {
    let kind = pad
        .current_caps()
        .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
        .unwrap_or_default();
    if !shared.cfg.want_video && !kind.starts_with("audio/") {
        discard(pipeline, pad)
    } else if kind == "video/x-raw" {
        if shared
            .video
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return Ok(());
        }
        video_branch(pipeline, pad, shared)
    } else if kind.starts_with("audio/") {
        audio_branch(pipeline, pad, shared)
    } else {
        Ok(())
    }
}

fn clock(d: Duration) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

impl Source {
    /// Open and preroll `path`. Fails when the file has no decodable video.
    pub fn open(path: &Path, cfg: SourceConfig) -> Result<Source, ExportError> {
        gst::init().map_err(|e| ExportError::Gst(e.to_string()))?;
        let pipeline = gst::Pipeline::with_name("klip-export-source");
        let src = make("filesrc")?;
        src.set_property("location", path.to_string_lossy().as_ref());
        let decode = make("decodebin")?;
        if !cfg.want_video {
            // Stop at raw audio: other streams are exposed undecoded and discarded.
            let caps = gst::Caps::builder("audio/x-raw").build();
            decode.set_property("caps", &caps);
        }
        pipeline.add_many([&src, &decode]).map_err(gst_err)?;
        src.link(&decode).map_err(gst_err)?;
        let shared = Arc::new(Shared {
            cfg,
            video: Mutex::new(None),
            audio: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            no_more_pads: AtomicBool::new(false),
        });
        let weak = pipeline.downgrade();
        let shared2 = shared.clone();
        decode.connect_pad_added(move |_, pad| {
            let Some(pipeline) = weak.upgrade() else {
                return;
            };
            match catch_unwind(AssertUnwindSafe(|| on_pad(&pipeline, pad, &shared2))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => shared2.fail(&e),
                Err(_) => shared2.fail("internal error while opening the clip"),
            }
        });
        let shared3 = shared.clone();
        decode.connect_no_more_pads(move |_| shared3.no_more_pads.store(true, Ordering::Release));
        let source = Source {
            pipeline,
            shared,
            segment: Mutex::new((0, 1.0)),
        };
        source
            .pipeline
            .set_state(gst::State::Paused)
            .map_err(|_| ExportError::Source("the clip could not be opened".into()))?;
        source.settle(gst::State::Paused, true)?;
        if cfg.want_video
            && source
                .shared
                .video
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none()
        {
            return Err(ExportError::Source("the clip has no video".into()));
        }
        source
            .shared
            .audio
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sort_by(|a, b| a.stream_id.cmp(&b.stream_id));
        source
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| ExportError::Source("the clip could not be played".into()))?;
        source.settle(gst::State::Playing, false)?;
        Ok(source)
    }

    /// How many audio tracks are read (in file order).
    pub fn audio_tracks(&self) -> usize {
        self.shared
            .audio
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn check(&self) -> Result<(), ExportError> {
        if let Some(e) = self
            .shared
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Err(ExportError::Source(e));
        }
        if let Some(bus) = self.pipeline.bus()
            && let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error])
            && let gst::MessageView::Error(e) = msg.view()
        {
            return Err(ExportError::Source(e.error().to_string()));
        }
        Ok(())
    }

    /// Wait until the pipeline sits in `target` with nothing pending (every sink,
    /// including the ones added while prerolling, has its first buffer) and, if asked,
    /// until the decoder has exposed all its streams.
    fn settle(&self, target: gst::State, all_pads: bool) -> Result<(), ExportError> {
        let deadline = Instant::now().checked_add(STEP);
        loop {
            self.check()?;
            let (res, current, pending) = self
                .pipeline
                .state(Some(gst::ClockTime::from_mseconds(100)));
            let pads_done = !all_pads || self.shared.no_more_pads.load(Ordering::Acquire);
            match res {
                Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll)
                    if current == target && pending == gst::State::VoidPending && pads_done =>
                {
                    return Ok(());
                }
                Err(_) => {
                    self.check()?;
                    return Err(ExportError::Source("the clip could not be decoded".into()));
                }
                _ => {}
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(ExportError::Source("the clip stopped answering".into()));
            }
        }
    }

    /// Play source `start_ms..end_ms` at `rate` (0.5, 1, 2): a flushing, accurate seek
    /// (running a little past `end_ms` so the audio tempo filter drains; the frame
    /// loop never picks frames past the end and the audio surplus is cut). Frames and
    /// audio of the previous segment are gone after this.
    pub fn seek(&self, start_ms: u64, end_ms: u64, rate: f64) -> Result<(), ExportError> {
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        let start = gst::ClockTime::from_mseconds(start_ms);
        let stop = gst::ClockTime::from_mseconds(
            end_ms
                .max(start_ms.saturating_add(1))
                .saturating_add(TAIL_MS),
        );
        self.pipeline
            .seek(
                rate,
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::SeekType::Set,
                start,
                gst::SeekType::Set,
                stop,
            )
            .map_err(|_| ExportError::Source("the clip cannot be seeked".into()))?;
        *self.segment.lock().unwrap_or_else(PoisonError::into_inner) = (start.nseconds(), rate);
        self.settle(gst::State::Playing, false)
    }

    fn video(&self) -> Result<gst_app::AppSink, ExportError> {
        self.shared
            .video
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| ExportError::Source("the clip has no video".into()))
    }

    /// The next decoded frame of the current segment; `None` at its end (always, for
    /// an audio-only source).
    pub fn next_frame(&self) -> Result<Option<Frame>, ExportError> {
        if !self.shared.cfg.want_video {
            return Ok(None);
        }
        let sink = self.video()?;
        loop {
            match sink.try_pull_sample(Some(clock(PULL))) {
                Some(sample) => {
                    let caps = sample
                        .caps()
                        .ok_or_else(|| ExportError::Source("frame without caps".into()))?;
                    let info = gst_video::VideoInfo::from_caps(caps)
                        .map_err(|_| ExportError::Source("frame with bad caps".into()))?;
                    let buffer = sample
                        .buffer_owned()
                        .ok_or_else(|| ExportError::Source("sample without a buffer".into()))?;
                    let Some(pts) = buffer.pts() else {
                        // A frame without a time can't be placed; skip it.
                        continue;
                    };
                    return Ok(Some(Frame {
                        pts_ms: pts.mseconds(),
                        buffer,
                        info,
                    }));
                }
                None if sink.is_eos() => return Ok(None),
                None => {
                    self.check()?;
                    return Err(ExportError::Source("the decoder stalled".into()));
                }
            }
        }
    }

    /// Everything the audio tracks produced for the current segment, in file order:
    /// interleaved stereo S16 at 48 kHz, starting at the segment's start (a track that
    /// begins later is padded with silence in front). Call after [`Self::next_frame`]
    /// returned `None`, so the segment is complete.
    pub fn drain_audio(&self) -> Result<Vec<Vec<i16>>, ExportError> {
        let sinks: Vec<gst_app::AppSink> = self
            .shared
            .audio
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|t| t.sink.clone())
            .collect();
        let (start_ns, rate) = *self.segment.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for sink in sinks {
            let mut samples: Vec<i16> = Vec::new();
            let mut first = true;
            loop {
                match sink.try_pull_sample(Some(clock(PULL))) {
                    Some(sample) => {
                        let Some(buffer) = sample.buffer() else {
                            continue;
                        };
                        if first {
                            first = false;
                            // Stream time of the first sample → output time of the gap.
                            let gap_ns = buffer
                                .pts()
                                .map_or(0, |p| p.nseconds().saturating_sub(start_ns));
                            let gap_ms = ((gap_ns as f64 / rate) / 1e6)
                                .round()
                                .clamp(0.0, MAX_LEAD_MS as f64)
                                as u64;
                            samples.resize(
                                usize::try_from(gap_ms)
                                    .unwrap_or(0)
                                    .saturating_mul(SAMPLES_PER_MS),
                                0,
                            );
                        }
                        if let Ok(map) = buffer.map_readable() {
                            samples.extend(
                                map.as_slice()
                                    .as_chunks::<2>()
                                    .0
                                    .iter()
                                    .map(|b| i16::from_le_bytes(*b)),
                            );
                        }
                    }
                    None if sink.is_eos() => break,
                    None => {
                        self.check()?;
                        return Err(ExportError::Source("the audio decoder stalled".into()));
                    }
                }
            }
            out.push(samples);
        }
        self.check()?;
        Ok(out)
    }
}
