//! The recorder: capture sessions (video, audio) feeding the replay ring, clip saves
//! (snapshot → MP4, no re-encode), status, and the command handler for the control
//! channel.
//!
//! Ring tracks: 0 is video; audio tracks follow in [`AudioConfig::track_labels`] order
//! (the mix first). Audio is best effort: if it can't start, video records anyway and
//! status reports why.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use klip_buffer::{PushError, Ring, RingConfig, Snapshot, SnapshotError};
use klip_capture::audio::{AudioConfig, AudioSession};
use klip_capture::klip_probe::EncoderChoice;
use klip_capture::{
    CaptureApi, CaptureError, CaptureSession, EncodeConfig, PacketSink, TimeBase, VideoConfig,
    VideoTarget, fit_size,
};
use klip_control::{CommandError, Handler, commands};
use klip_media::{NANOS_PER_MILLI, Nanos, Packet, TrackInfo, TrackKind, nanos_to_millis};
use klip_mp4::{Mp4Error, Mp4Options, Mp4Track, save_mp4_atomic};
use serde_json::{Value, json};

/// Errors from the recorder.
#[derive(Debug, thiserror::Error)]
pub enum RecorderError {
    #[error("capture: {0}")]
    Capture(#[from] CaptureError),
    #[error("{0}")]
    Snapshot(#[from] SnapshotError),
    #[error("writing clip: {0}")]
    Mp4(#[from] Mp4Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid configuration: {0}")]
    Config(String),
}

/// Recorder settings.
#[derive(Clone, Debug)]
pub struct RecorderConfig {
    /// Replay buffer length.
    pub buffer: Nanos,
    /// Hard cap on buffered bytes.
    pub max_buffer_bytes: u64,
    /// Folder for saved clips (created if missing).
    pub clip_dir: PathBuf,
    pub video: VideoConfig,
    pub encode: EncodeConfig,
    /// Encoders to try, in order, if `encode.encoder` fails to start (same settings).
    pub encoder_fallbacks: Vec<EncoderChoice>,
    /// `None`: no audio.
    pub audio: Option<AudioConfig>,
}

/// One encoder tried while starting a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderAttempt {
    pub element: String,
    /// Why it was given up; `None` for the one that runs.
    pub error: Option<String>,
}

/// How long a new capture may take to deliver its first encoded frame. A pipeline that
/// reports an error before that is given up and the next encoder is tried; one that is
/// merely slow (a window that hasn't drawn yet) is kept.
const FIRST_PACKET_TIMEOUT: Duration = Duration::from_secs(3);

/// Bytes needed to hold `buffer` of video at `bitrate_kbps` (VBR peaks at 1.5x) plus one
/// GOP of slack. Used for the default byte cap and the RAM estimate.
pub fn estimate_buffer_bytes(buffer: Nanos, bitrate_kbps: u32) -> u64 {
    let secs = u64::try_from(buffer.max(0)).unwrap_or(0) / 1_000_000_000;
    let bytes_per_sec = u64::from(bitrate_kbps).saturating_mul(1000) / 8;
    // 1.5x VBR peak, +1 s for the GOP that eviction keeps.
    bytes_per_sec
        .saturating_mul(3)
        .saturating_div(2)
        .saturating_mul(secs.saturating_add(1))
}

/// Longest a save waits for audio to reach the video's end (audio arrives a few tens of
/// milliseconds after video).
pub const AUDIO_CATCH_UP: Duration = Duration::from_millis(250);

/// Audio tracks whose newest packet is older than this before the video's end are treated
/// as stalled; saves don't wait for them.
const STALLED_TRACK: Nanos = 2_000_000_000;

/// A replay ring that a capture pipeline can push into.
pub struct RingSink {
    ring: Mutex<Ring>,
    last_error: Mutex<Option<String>>,
    last_warning: Mutex<Option<String>>,
    rejected_unexpected: AtomicU64,
    /// Bit `i` set once track `i` delivered its first packet (for the debug log).
    seen_tracks: AtomicU64,
}

impl std::fmt::Debug for RingSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RingSink").finish_non_exhaustive()
    }
}

impl RingSink {
    /// A ring with one video track (track 0).
    pub fn new(cfg: RingConfig) -> Result<RingSink, RecorderError> {
        RingSink::with_audio(cfg, 0)
    }

    /// A ring with a video track (0) and `audio_tracks` audio tracks (1..).
    pub fn with_audio(cfg: RingConfig, audio_tracks: usize) -> Result<RingSink, RecorderError> {
        let mut kinds = vec![TrackKind::Video];
        kinds.extend(std::iter::repeat_n(TrackKind::Audio, audio_tracks));
        let ring = Ring::new(cfg, &kinds)
            .ok_or_else(|| RecorderError::Config("ring needs a video track".into()))?;
        Ok(RingSink {
            ring: Mutex::new(ring),
            last_error: Mutex::new(None),
            last_warning: Mutex::new(None),
            rejected_unexpected: AtomicU64::new(0),
            seen_tracks: AtomicU64::new(0),
        })
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Copy out the last `length` (cheap: payloads are refcounted).
    pub fn snapshot(&self, length: Nanos) -> Result<Snapshot, SnapshotError> {
        self.ring().snapshot(length)
    }

    /// Copy out the last `length` up to the video packet that starts before `until`.
    pub fn snapshot_until(
        &self,
        until: Option<Nanos>,
        length: Nanos,
    ) -> Result<Snapshot, SnapshotError> {
        self.ring().snapshot_until(until, length)
    }

    /// End of the buffered video.
    pub fn video_end(&self) -> Option<Nanos> {
        self.ring().anchor_end()
    }

    /// Wait until every live audio track reaches `until`, at most `timeout`. Returns the
    /// time waited.
    pub fn wait_for_audio(&self, until: Nanos, timeout: Duration) -> Duration {
        let t0 = Instant::now();
        loop {
            let behind = {
                let ring = self.ring();
                ring.active_secondary_tracks().into_iter().any(|i| {
                    ring.track_end(i)
                        .is_some_and(|e| e < until && e > until.saturating_sub(STALLED_TRACK))
                })
            };
            let waited = t0.elapsed();
            if !behind || waited >= timeout {
                return waited;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn stats(&self) -> klip_buffer::RingStats {
        self.ring().stats()
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Forget the last error (after a failed start that was recovered from).
    pub fn clear_error(&self) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub fn last_warning(&self) -> Option<String> {
        self.last_warning
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl PacketSink for RingSink {
    fn on_track_info(&self, track: usize, info: TrackInfo) {
        if let Err(e) = self.ring().set_track_info(track, info) {
            self.on_error(format!("track description rejected: {e}"));
        }
    }

    fn on_packet(&self, track: usize, packet: Packet) {
        let bit = u32::try_from(track)
            .ok()
            .and_then(|t| 1u64.checked_shl(t))
            .unwrap_or(0);
        if bit != 0 && self.seen_tracks.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            tracing::debug!(track, pts = packet.pts, "first packet");
        }
        match self.ring().push(track, packet) {
            Ok(()) | Err(PushError::WaitingForKeyframe) => {}
            Err(e) => {
                self.rejected_unexpected.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(error = %e, "packet rejected");
            }
        }
    }

    fn on_error(&self, message: String) {
        tracing::error!(error = %message, "capture error");
        *self
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(message);
    }

    fn on_warning(&self, message: String) {
        tracing::warn!(warning = %message, "capture warning");
        *self
            .last_warning
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(message);
    }
}

/// A saved clip.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedClip {
    pub path: PathBuf,
    pub duration: Nanos,
    pub bytes: u64,
    pub frames: usize,
    pub audio_tracks: usize,
    /// Running time of the clip's first frame (clip time 0) on the recorder's timeline.
    pub start: Nanos,
    /// Wall time from the request to the renamed file, in milliseconds.
    pub save_ms: f64,
    /// Part of `save_ms` spent waiting for audio to catch up with video.
    pub audio_wait_ms: f64,
}

/// Save the last `length` of `sink` into `dir` as a new MP4. `names[i]` names ring track
/// `i` in the file. The first audio track is the one players play; the others are
/// written disabled (editors still import them).
pub fn save_from(
    sink: &RingSink,
    dir: &Path,
    length: Nanos,
    names: &[String],
) -> Result<SavedClip, RecorderError> {
    let t0 = Instant::now();
    // The clip ends where it was requested; audio arrives a little later than video, so
    // give it a moment to cover that end.
    let until = sink.video_end();
    let waited = match until {
        Some(u) => sink.wait_for_audio(u, AUDIO_CATCH_UP),
        None => Duration::ZERO,
    };
    let snap = sink.snapshot_until(until, length)?;
    std::fs::create_dir_all(dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = unique_clip_path(dir)?;
    let mut audio_seen = false;
    let tracks: Vec<Mp4Track<'_>> = snap
        .tracks
        .iter()
        .map(|t| {
            let mut m = Mp4Track::new(&t.info, &t.packets);
            if let Some(name) = names.get(t.track).filter(|n| !n.is_empty()) {
                m = m.named(name);
            }
            if t.info.kind() == TrackKind::Audio {
                m = m.enabled(!audio_seen);
                audio_seen = true;
            }
            m
        })
        .collect();
    let audio_tracks = snap
        .tracks
        .iter()
        .filter(|t| t.info.kind() == TrackKind::Audio)
        .count();
    let summary = save_mp4_atomic(
        &path,
        &tracks,
        &Mp4Options {
            creation_time_unix: now,
            force_co64: false,
        },
    )?;
    let frames = snap.tracks.first().map(|t| t.packets.len()).unwrap_or(0);
    Ok(SavedClip {
        path,
        duration: summary.duration,
        bytes: summary.bytes,
        frames,
        audio_tracks,
        start: snap.start,
        save_ms: t0.elapsed().as_secs_f64() * 1000.0,
        audio_wait_ms: waited.as_secs_f64() * 1000.0,
    })
}

fn unique_clip_path(dir: &Path) -> Result<PathBuf, RecorderError> {
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d_%H-%M-%S").to_string();
    for n in 1u32..1000 {
        let name = if n == 1 {
            format!("Klip_{stamp}.mp4")
        } else {
            format!("Klip_{stamp}_{n}.mp4")
        };
        let p = dir.join(name);
        if !p.exists() {
            return Ok(p);
        }
    }
    Err(RecorderError::Config(
        "could not find a free clip file name".into(),
    ))
}

/// A running recorder.
pub struct Recorder {
    sink: Arc<RingSink>,
    video: Arc<VideoCtl>,
    supervisor: Mutex<Option<(Arc<AtomicBool>, JoinHandle<()>)>>,
    audio: Mutex<Option<AudioSession>>,
    /// Why audio isn't recording, if it was requested and failed to start.
    audio_error: Option<String>,
    /// Encoders tried at start, the running one last.
    encoder_attempts: Vec<EncoderAttempt>,
    /// Name of each ring track.
    track_names: Vec<String>,
    timebase: TimeBase,
    cfg: RecorderConfig,
    started: Instant,
    output: (u32, u32),
    stop_requested: AtomicBool,
    save_lock: Mutex<()>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

/// Start the video pipeline with the first encoder of `cfg.encode` + fallbacks that
/// works: one that fails to build, to start, or reports an error before its first
/// packet is stopped and the next is tried. Errors that no encoder can fix (the target
/// window is gone, bad settings) end the search at once.
fn start_video(
    cfg: &RecorderConfig,
    sink: &Arc<RingSink>,
    timebase: &TimeBase,
) -> Result<(CaptureSession, EncodeConfig, Vec<EncoderAttempt>), RecorderError> {
    let mut attempts: Vec<EncoderAttempt> = Vec::new();
    let mut last_err: Option<CaptureError> = None;
    let encoders =
        std::iter::once(cfg.encode.encoder.clone()).chain(cfg.encoder_fallbacks.iter().cloned());
    for encoder in encoders {
        if attempts.iter().any(|a| a.element == encoder.element) {
            continue;
        }
        let encode = EncodeConfig {
            encoder,
            ..cfg.encode.clone()
        };
        let element = encode.encoder.element.clone();
        let sink_dyn: Arc<dyn PacketSink> = sink.clone();
        let session = match CaptureSession::start(&cfg.video, &encode, sink_dyn, Some(timebase)) {
            Ok(s) => s,
            Err(e @ (CaptureError::Target(_) | CaptureError::Platform(_))) => return Err(e.into()),
            Err(e) => {
                tracing::warn!(encoder = %element, error = %e, "encoder failed to start; trying the next");
                attempts.push(EncoderAttempt {
                    element,
                    error: Some(e.to_string()),
                });
                last_err = Some(e);
                continue;
            }
        };
        let t0 = Instant::now();
        let failed = loop {
            let st = session.stats();
            if st.errors.load(Ordering::Relaxed) > 0 {
                break true;
            }
            if st.packets.load(Ordering::Relaxed) > 0 {
                break false;
            }
            if t0.elapsed() >= FIRST_PACKET_TIMEOUT {
                tracing::warn!(encoder = %element, "no video yet; keeping this encoder");
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !failed {
            tracing::info!(encoder = %element, first_packet_ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX), "video started");
            attempts.push(EncoderAttempt {
                element,
                error: None,
            });
            sink.clear_error();
            return Ok((session, encode, attempts));
        }
        let error = sink
            .last_error()
            .unwrap_or_else(|| "pipeline error".to_string());
        session.stop();
        tracing::warn!(encoder = %element, error = %error, "encoder failed at start; trying the next");
        last_err = Some(CaptureError::Gst(error.clone()));
        attempts.push(EncoderAttempt {
            element,
            error: Some(error),
        });
    }
    Err(last_err
        .map(RecorderError::from)
        .unwrap_or_else(|| RecorderError::Config("no encoder to try".into())))
}

impl Recorder {
    /// Encoders tried at start, the running one last (see [`start_video`]).
    pub fn encoder_attempts(&self) -> &[EncoderAttempt] {
        &self.encoder_attempts
    }

    /// Start capturing into a new replay ring.
    pub fn start(cfg: RecorderConfig) -> Result<Recorder, RecorderError> {
        if cfg.buffer <= 0 {
            return Err(RecorderError::Config(
                "buffer length must be positive".into(),
            ));
        }
        let audio_labels = cfg
            .audio
            .as_ref()
            .map(AudioConfig::track_labels)
            .unwrap_or_default();
        let sink = Arc::new(RingSink::with_audio(
            RingConfig {
                max_duration: cfg.buffer,
                max_bytes: cfg.max_buffer_bytes,
            },
            audio_labels.len(),
        )?);
        // One clock and base time for every pipeline keeps audio and video in sync.
        let timebase = TimeBase::new()?;
        let mut cfg = cfg;
        let (session, encode, encoder_attempts, mode) = match start_video(&cfg, &sink, &timebase) {
            Ok((s, e, a)) => {
                let mode = match cfg.video.target {
                    VideoTarget::Window(hwnd) => VideoMode::Window { hwnd },
                    VideoTarget::Monitor(monitor) => VideoMode::Monitor { monitor },
                    VideoTarget::PrimaryMonitor => VideoMode::Monitor { monitor: 0 },
                };
                (s, e, a, mode)
            }
            // A window that can't be captured: record the monitor it is on instead, at
            // the size the window would have had.
            Err(e) => match window_fallback(&cfg) {
                Some((monitor_cfg, mode)) => {
                    tracing::warn!(error = %e, "window capture failed; capturing its monitor");
                    let mut c = cfg.clone();
                    c.video = monitor_cfg;
                    let (s, enc, a) = start_video(&c, &sink, &timebase)?;
                    cfg.video.output = c.video.output;
                    (s, enc, a, mode)
                }
                None => return Err(e),
            },
        };
        cfg.encode = encode;
        let output = session.output_size();
        // Later restarts keep the encoder and the output size, so the replay track (and
        // what's buffered) carries on.
        cfg.video.output = Some(output);
        let video = Arc::new(VideoCtl {
            session: Mutex::new(Some(session)),
            mode: Mutex::new(mode),
            switches: Mutex::new(Vec::new()),
            video_cfg: cfg.video.clone(),
            encode: cfg.encode.clone(),
            sink: sink.clone(),
            timebase: timebase.clone(),
            started: Instant::now(),
        });
        let supervisor = if matches!(mode, VideoMode::Monitor { .. }) {
            None
        } else {
            let stop = Arc::new(AtomicBool::new(false));
            let (v, st) = (video.clone(), stop.clone());
            std::thread::Builder::new()
                .name("klip-video-supervisor".into())
                .spawn(move || {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        supervise(&v, &st);
                    }));
                })
                .ok()
                .map(|h| (stop, h))
        };
        let (audio, audio_error) = match &cfg.audio {
            None => (None, None),
            Some(a) => match AudioSession::start(a, 1, sink.clone(), Some(&timebase)) {
                Ok(s) => (Some(s), None),
                Err(e) => {
                    tracing::error!(error = %e, "audio could not start; recording video only");
                    (None, Some(e.to_string()))
                }
            },
        };
        let mut track_names = vec![String::new()];
        track_names.extend(audio_labels);
        Ok(Recorder {
            sink,
            video,
            supervisor: Mutex::new(supervisor),
            audio: Mutex::new(audio),
            audio_error,
            encoder_attempts,
            track_names,
            timebase,
            cfg,
            started: Instant::now(),
            output,
            stop_requested: AtomicBool::new(false),
            save_lock: Mutex::new(()),
        })
    }

    /// Save the last `length` to the clip folder.
    pub fn save_clip(&self, length: Nanos) -> Result<SavedClip, RecorderError> {
        let _guard = self
            .save_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let clip = save_from(&self.sink, &self.cfg.clip_dir, length, &self.track_names)?;
        tracing::info!(
            ms = clip.save_ms,
            bytes = clip.bytes,
            frames = clip.frames,
            audio_tracks = clip.audio_tracks,
            audio_wait_ms = clip.audio_wait_ms,
            "clip saved"
        );
        Ok(clip)
    }

    /// Status as JSON (also the `recorder.status` result).
    pub fn status(&self) -> Value {
        let s = self.sink.stats();
        let session = self
            .video
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (packets, errors, encoder_drops) = session
            .as_ref()
            .map(|c| {
                (
                    c.stats().packets.load(Ordering::Relaxed),
                    c.stats().errors.load(Ordering::Relaxed),
                    c.stats().encoder_queue_drops.load(Ordering::Relaxed),
                )
            })
            .unwrap_or((0, 0, 0));
        let capture_api = session.as_ref().map(|c| format!("{:?}", c.api()));
        #[cfg(windows)]
        let gpu = session.as_ref().and_then(|c| c.gpu_stats()).map(|g| {
            json!({
                "frames": g.frames.load(Ordering::Relaxed),
                "dropped_busy": g.dropped_busy.load(Ordering::Relaxed),
                "skipped": g.skipped.load(Ordering::Relaxed),
                "errors": g.errors.load(Ordering::Relaxed),
                "resizes": g.resizes.load(Ordering::Relaxed),
                "scene_peak_nits": f32::from_bits(g.scene_peak_nits.load(Ordering::Relaxed)),
                "wgc_lead_us": {
                    "mean": g.lead_sum_us.load(Ordering::Relaxed).checked_div(
                        i64::try_from(g.lead_count.load(Ordering::Relaxed)).unwrap_or(0),
                    ),
                    "min": g.lead_min_us.load(Ordering::Relaxed),
                    "max": g.lead_max_us.load(Ordering::Relaxed),
                },
            })
        });
        #[cfg(not(windows))]
        let gpu: Option<Value> = None;
        drop(session);
        json!({
            "buffered_ms": nanos_to_millis(s.buffered),
            "buffer_ms": nanos_to_millis(self.cfg.buffer),
            "bytes": s.bytes,
            "max_bytes": self.cfg.max_buffer_bytes,
            "packets": s.packets,
            "keyframes": s.keyframes,
            "rejected": s.rejected,
            "evicted": s.evicted,
            "encoder": self.cfg.encode.encoder.element,
            "encoder_attempts": self.encoder_attempts.iter().map(|a| json!({
                "element": a.element,
                "error": a.error,
            })).collect::<Vec<_>>(),
            "codec": self.cfg.encode.encoder.codec.name(),
            "bitrate_kbps": self.cfg.encode.bitrate_kbps,
            "width": self.output.0,
            "height": self.output.1,
            "fps": self.cfg.video.fps,
            "capture_packets": packets,
            "capture_errors": errors,
            "encoder_queue_drops": encoder_drops,
            "capture_api": capture_api,
            "video": self.video.status(),
            "capture": gpu,
            "last_error": self.sink.last_error(),
            "last_warning": self.sink.last_warning(),
            "audio": self.audio_status(),
            "clock_base_ns": self.timebase.base_clock_ns(),
            "uptime_ms": u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }

    fn audio_status(&self) -> Value {
        let Some(cfg) = &self.cfg.audio else {
            return Value::Null;
        };
        let audio = self.audio.lock().unwrap_or_else(PoisonError::into_inner);
        let process_lags: Vec<Value> = audio
            .as_ref()
            .map(|a| {
                a.process_lags()
                    .into_iter()
                    .map(|l| json!({ "label": l.label, "lag_ms": l.lag_ms, "measurements": l.measurements }))
                    .collect()
            })
            .unwrap_or_default();
        let counters = audio.as_ref().and_then(|a| a.stats()).map(|st| {
            json!({
                "packets": st.packets.load(Ordering::Relaxed),
                "errors": st.errors.load(Ordering::Relaxed),
                "warnings": st.warnings.load(Ordering::Relaxed),
            })
        });
        json!({
            "tracks": self.track_names.get(1..).unwrap_or_default(),
            "inputs": cfg.inputs.iter().map(|i| json!({
                "label": i.label,
                "source": i.source.describe(),
            })).collect::<Vec<_>>(),
            "bitrate": cfg.bitrate,
            "running": counters.is_some(),
            "process_lag": process_lags,
            "counters": counters,
            "error": self.audio_error,
        })
    }

    /// Ask the owning process to stop (used by `recorder.stop`).
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }

    /// Stop capturing. Buffered data stays available for saves until drop.
    pub fn stop_capture(&self) {
        let supervisor = self
            .supervisor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((stop, handle)) = supervisor {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        let audio = self
            .audio
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(a) = audio {
            a.stop();
        }
        let session = self
            .video
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(s) = session {
            s.stop();
        }
    }
}

/// For a window target: capture settings for the monitor the window is on, with the
/// output size the window would have had, and the mode that records that.
fn window_fallback(cfg: &RecorderConfig) -> Option<(VideoConfig, VideoMode)> {
    let VideoTarget::Window(hwnd) = cfg.video.target else {
        return None;
    };
    let w = klip_platform::window_info(hwnd).ok()?;
    if w.minimized || w.client_width == 0 || w.client_height == 0 {
        return None;
    }
    let output = fit_size(
        w.client_width,
        w.client_height,
        cfg.video.max_width,
        cfg.video.max_height,
    );
    let video = VideoConfig {
        target: VideoTarget::Monitor(w.monitor),
        output: Some(output),
        ..cfg.video.clone()
    };
    Some((
        video,
        VideoMode::WindowOnMonitor {
            hwnd,
            monitor: w.monitor,
            dxgi: false,
        },
    ))
}

/// One capture switch, for the status.
#[derive(Clone, Debug)]
struct SwitchRecord {
    at_ms: u64,
    to: VideoMode,
    reason: &'static str,
    ok: bool,
}

/// The video capture, restartable by the supervisor.
struct VideoCtl {
    session: Mutex<Option<CaptureSession>>,
    mode: Mutex<VideoMode>,
    switches: Mutex<Vec<SwitchRecord>>,
    /// Settings with the output size pinned.
    video_cfg: VideoConfig,
    encode: EncodeConfig,
    sink: Arc<RingSink>,
    timebase: TimeBase,
    started: Instant,
}

fn mode_json(m: VideoMode) -> Value {
    match m {
        VideoMode::Window { hwnd } => json!({ "kind": "window", "window": hwnd }),
        VideoMode::WindowOnMonitor {
            hwnd,
            monitor,
            dxgi,
        } => json!({
            "kind": "window_on_monitor", "window": hwnd, "monitor": monitor, "dxgi": dxgi,
        }),
        VideoMode::Monitor { monitor } => json!({ "kind": "monitor", "monitor": monitor }),
    }
}

impl VideoCtl {
    fn status(&self) -> Value {
        let mode = *self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        let switches: Vec<Value> = self
            .switches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|s| json!({ "at_ms": s.at_ms, "to": mode_json(s.to), "reason": s.reason, "ok": s.ok }))
            .collect();
        json!({ "mode": mode_json(mode), "switches": switches })
    }

    fn config_for(&self, mode: VideoMode) -> VideoConfig {
        let (target, api) = match mode {
            VideoMode::Window { hwnd } => (VideoTarget::Window(hwnd), self.video_cfg.api),
            VideoMode::WindowOnMonitor { monitor, dxgi, .. } => (
                VideoTarget::Monitor(monitor),
                if dxgi {
                    CaptureApi::GstDxgi
                } else {
                    self.video_cfg.api
                },
            ),
            VideoMode::Monitor { monitor } => (VideoTarget::Monitor(monitor), self.video_cfg.api),
        };
        VideoConfig {
            target,
            api,
            ..self.video_cfg.clone()
        }
    }

    /// Restart video capture in `to`; on failure go back to the current mode.
    fn switch(&self, to: VideoMode, reason: &'static str) {
        let from = *self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        tracing::info!(?from, ?to, reason, "switching video capture");
        let mut guard = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = guard.take() {
            old.stop();
        }
        self.sink.clear_error();
        let sink: Arc<dyn PacketSink> = self.sink.clone();
        let started = CaptureSession::start(
            &self.config_for(to),
            &self.encode,
            sink.clone(),
            Some(&self.timebase),
        );
        let ok = started.is_ok();
        match started {
            Ok(s) => {
                *guard = Some(s);
                *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = to;
            }
            Err(e) => {
                tracing::warn!(error = %e, "switch failed; going back");
                match CaptureSession::start(
                    &self.config_for(from),
                    &self.encode,
                    sink,
                    Some(&self.timebase),
                ) {
                    Ok(s) => *guard = Some(s),
                    Err(e) => tracing::error!(error = %e, "video capture could not restart"),
                }
            }
        }
        drop(guard);
        let at_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut log = self.switches.lock().unwrap_or_else(PoisonError::into_inner);
        if log.len() >= 32 {
            log.remove(0);
        }
        log.push(SwitchRecord {
            at_ms,
            to,
            reason,
            ok,
        });
    }
}

/// Watch the capture twice a second and switch per [`supervisor::decide`].
fn supervise(video: &VideoCtl, stop: &AtomicBool) {
    let tick = Duration::from_millis(500);
    let mut last_frames: Option<u64> = None;
    let mut last_change = Instant::now();
    let mut last_switch = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(tick);
        let mode = *video.mode.lock().unwrap_or_else(PoisonError::into_inner);
        let hwnd = match mode {
            VideoMode::Window { hwnd } | VideoMode::WindowOnMonitor { hwnd, .. } => hwnd,
            VideoMode::Monitor { .. } => return,
        };
        let (frames, failed) = {
            let s = video.session.lock().unwrap_or_else(PoisonError::into_inner);
            match s.as_ref() {
                Some(c) => (c.frames_captured(), c.failed()),
                None => (0, true),
            }
        };
        if last_frames != Some(frames) {
            last_frames = Some(frames);
            last_change = Instant::now();
        }
        let window = klip_platform::window_info(hwnd).ok();
        let monitors = klip_platform::list_monitors().unwrap_or_default();
        let covers = window.as_ref().is_some_and(|w| {
            monitors
                .iter()
                .find(|m| m.handle == w.monitor)
                .is_some_and(|m| w.covers(m))
        });
        let foreground = match (window.as_ref(), klip_platform::foreground_window()) {
            (Some(w), Some(f)) => {
                f == hwnd || klip_platform::window_info(f).is_ok_and(|fw| fw.pid == w.pid)
            }
            _ => false,
        };
        let obs = Observation {
            stalled: last_change.elapsed(),
            failed,
            window_exists: window.is_some(),
            minimized: window.as_ref().is_some_and(|w| w.minimized),
            covers_monitor: covers,
            window_monitor: window.as_ref().map(|w| w.monitor),
            foreground,
            exclusive: foreground && klip_platform::exclusive_fullscreen_active(),
        };
        if let Action::Switch { to, reason } = decide(mode, &obs)
            && last_switch.elapsed() >= MIN_DWELL
        {
            video.switch(to, reason);
            last_switch = Instant::now();
            last_frames = None;
            last_change = Instant::now();
        }
    }
}

pub mod supervisor;
use supervisor::{Action, MIN_DWELL, Observation, VideoMode, decide};

/// Smallest and largest clip length accepted over the control channel.
const MIN_CLIP_MS: u64 = 1_000;
const MAX_CLIP_MS: u64 = 3_600_000;

impl Handler for Recorder {
    fn handle(&self, cmd: &str, args: &Value) -> Result<Value, CommandError> {
        match cmd {
            commands::PING => Ok(json!({ "pong": true })),
            commands::RECORDER_STATUS => Ok(self.status()),
            commands::RECORDER_STOP => {
                self.request_stop();
                Ok(json!({ "stopping": true }))
            }
            commands::CLIP_SAVE => {
                let last_ms = match args.get("last_ms") {
                    None | Some(Value::Null) => 30_000,
                    Some(v) => v.as_u64().ok_or_else(|| {
                        CommandError::bad_args("last_ms must be a positive integer")
                    })?,
                };
                if !(MIN_CLIP_MS..=MAX_CLIP_MS).contains(&last_ms) {
                    return Err(CommandError::bad_args(format!(
                        "last_ms must be between {MIN_CLIP_MS} and {MAX_CLIP_MS}"
                    )));
                }
                let ns = i64::try_from(last_ms)
                    .unwrap_or(0)
                    .saturating_mul(NANOS_PER_MILLI);
                let clip = self.save_clip(ns).map_err(|e| match e {
                    RecorderError::Snapshot(SnapshotError::Empty) => {
                        CommandError::new("not_ready", "the replay buffer is still empty")
                    }
                    other => CommandError::new("failed", other.to_string()),
                })?;
                Ok(json!({
                    "path": clip.path.to_string_lossy(),
                    "duration_ms": nanos_to_millis(clip.duration),
                    "bytes": clip.bytes,
                    "frames": clip.frames,
                    "audio_tracks": clip.audio_tracks,
                    "start_ns": clip.start,
                    "save_ms": clip.save_ms,
                    "audio_wait_ms": clip.audio_wait_ms,
                }))
            }
            other => Err(CommandError::unknown(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use klip_media::{Codec, TrackParams, VideoParams};

    use super::*;

    fn info() -> TrackInfo {
        TrackInfo {
            codec: Codec::H264,
            codec_data: vec![1u8, 0x64, 0, 0x28].into(),
            params: TrackParams::Video(VideoParams {
                width: 64,
                height: 64,
                colorimetry: None,
            }),
            bitrate: None,
        }
    }

    fn packet(i: i64) -> Packet {
        Packet {
            pts: i * 16_666_667,
            dts: i * 16_666_667,
            duration: 16_666_667,
            keyframe: i % 60 == 0,
            data: vec![0u8; 100].into(),
        }
    }

    #[test]
    fn ring_sink_saves_unique_atomic_clips() {
        let dir = tempfile::tempdir().unwrap();
        let sink = RingSink::new(RingConfig {
            max_duration: 10 * 1_000_000_000,
            max_bytes: u64::MAX,
        })
        .unwrap();
        assert!(matches!(
            save_from(&sink, dir.path(), 1_000_000_000, &[]),
            Err(RecorderError::Snapshot(SnapshotError::Empty))
        ));
        sink.on_track_info(0, info());
        // Delta frames before the first keyframe are dropped silently.
        sink.on_packet(
            0,
            Packet {
                keyframe: false,
                ..packet(0)
            },
        );
        for i in 1..=300 {
            sink.on_packet(0, packet(i));
        }
        let a = save_from(&sink, dir.path(), 2_000_000_000, &[]).unwrap();
        let b = save_from(&sink, dir.path(), 2_000_000_000, &[]).unwrap();
        assert_ne!(a.path, b.path);
        assert!(a.path.exists() && b.path.exists());
        assert_eq!(a.frames, 121, "from keyframe 180 to frame 300");
        assert_eq!(std::fs::metadata(&a.path).unwrap().len(), a.bytes);
        assert_eq!(sink.rejected_unexpected.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn audio_tracks_are_saved_named_with_only_the_first_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let sink = RingSink::with_audio(
            RingConfig {
                max_duration: 10 * 1_000_000_000,
                max_bytes: u64::MAX,
            },
            3,
        )
        .unwrap();
        sink.on_track_info(0, info());
        let aac = TrackInfo {
            codec: Codec::Aac,
            codec_data: vec![0x11u8, 0x90].into(),
            params: TrackParams::Audio(klip_media::AudioParams {
                sample_rate: 48_000,
                channels: 2,
            }),
            bitrate: Some(160_000),
        };
        // Track 2 never gets a description (e.g. its device failed before any data).
        sink.on_track_info(1, aac.clone());
        sink.on_track_info(3, aac);
        for i in 0..=120 {
            sink.on_packet(0, packet(i));
        }
        let step = 21_333_333;
        for k in 0..90 {
            for track in [1, 3] {
                sink.on_packet(
                    track,
                    Packet {
                        pts: k * step,
                        dts: k * step,
                        duration: step,
                        keyframe: true,
                        data: vec![0u8; 50].into(),
                    },
                );
            }
        }
        let names: Vec<String> = ["", "Mix", "Game", "Microphone"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let clip = save_from(&sink, dir.path(), 1_000_000_000, &names).unwrap();
        assert_eq!(clip.audio_tracks, 2);
        let data = std::fs::read(&clip.path).unwrap();
        let has = |needle: &[u8]| data.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"Mix\0") && has(b"Microphone\0") && !has(b"Game\0"));
        assert_eq!(sink.rejected_unexpected.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn buffer_estimate() {
        // 120 s at 20 Mbps: 2.5 MB/s * 1.5 * 121 s.
        assert_eq!(
            estimate_buffer_bytes(120 * 1_000_000_000, 20_000),
            453_750_000
        );
        assert_eq!(estimate_buffer_bytes(-5, 20_000), 3_750_000);
    }
}
