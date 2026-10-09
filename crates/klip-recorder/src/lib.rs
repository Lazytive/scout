//! The recorder: a capture session feeding the replay ring, clip saves (snapshot → MP4,
//! no re-encode), status, and the command handler for the control channel.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use klip_buffer::{PushError, Ring, RingConfig, Snapshot, SnapshotError};
use klip_capture::{CaptureError, CaptureSession, EncodeConfig, PacketSink, VideoConfig};
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
}

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

/// A replay ring that a capture pipeline can push into.
pub struct RingSink {
    ring: Mutex<Ring>,
    last_error: Mutex<Option<String>>,
    rejected_unexpected: AtomicU64,
}

impl std::fmt::Debug for RingSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RingSink").finish_non_exhaustive()
    }
}

impl RingSink {
    /// A ring with one video track (track 0).
    pub fn new(cfg: RingConfig) -> Result<RingSink, RecorderError> {
        let ring = Ring::new(cfg, &[TrackKind::Video])
            .ok_or_else(|| RecorderError::Config("ring needs a video track".into()))?;
        Ok(RingSink {
            ring: Mutex::new(ring),
            last_error: Mutex::new(None),
            rejected_unexpected: AtomicU64::new(0),
        })
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Copy out the last `length` (cheap: payloads are refcounted).
    pub fn snapshot(&self, length: Nanos) -> Result<Snapshot, SnapshotError> {
        self.ring().snapshot(length)
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
}

impl PacketSink for RingSink {
    fn on_track_info(&self, track: usize, info: TrackInfo) {
        if let Err(e) = self.ring().set_track_info(track, info) {
            self.on_error(format!("track description rejected: {e}"));
        }
    }

    fn on_packet(&self, track: usize, packet: Packet) {
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
}

/// A saved clip.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedClip {
    pub path: PathBuf,
    pub duration: Nanos,
    pub bytes: u64,
    pub frames: usize,
    /// Wall time from the request to the renamed file, in milliseconds.
    pub save_ms: f64,
}

/// Save the last `length` of `sink` into `dir` as a new MP4.
pub fn save_from(sink: &RingSink, dir: &Path, length: Nanos) -> Result<SavedClip, RecorderError> {
    let t0 = Instant::now();
    let snap = sink.snapshot(length)?;
    std::fs::create_dir_all(dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = unique_clip_path(dir)?;
    let tracks: Vec<Mp4Track<'_>> = snap
        .tracks
        .iter()
        .map(|t| Mp4Track {
            info: &t.info,
            packets: &t.packets,
        })
        .collect();
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
        save_ms: t0.elapsed().as_secs_f64() * 1000.0,
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
    session: Mutex<Option<CaptureSession>>,
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

impl Recorder {
    /// Start capturing into a new replay ring.
    pub fn start(cfg: RecorderConfig) -> Result<Recorder, RecorderError> {
        if cfg.buffer <= 0 {
            return Err(RecorderError::Config(
                "buffer length must be positive".into(),
            ));
        }
        let sink = Arc::new(RingSink::new(RingConfig {
            max_duration: cfg.buffer,
            max_bytes: cfg.max_buffer_bytes,
        })?);
        let session = CaptureSession::start(&cfg.video, &cfg.encode, sink.clone())?;
        let output = session.output_size();
        Ok(Recorder {
            sink,
            session: Mutex::new(Some(session)),
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
        let clip = save_from(&self.sink, &self.cfg.clip_dir, length)?;
        tracing::info!(
            ms = clip.save_ms,
            bytes = clip.bytes,
            frames = clip.frames,
            "clip saved"
        );
        Ok(clip)
    }

    /// Status as JSON (also the `recorder.status` result).
    pub fn status(&self) -> Value {
        let s = self.sink.stats();
        let session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let (packets, errors) = session
            .as_ref()
            .map(|c| {
                (
                    c.stats().packets.load(Ordering::Relaxed),
                    c.stats().errors.load(Ordering::Relaxed),
                )
            })
            .unwrap_or((0, 0));
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
            "codec": self.cfg.encode.encoder.codec.name(),
            "bitrate_kbps": self.cfg.encode.bitrate_kbps,
            "width": self.output.0,
            "height": self.output.1,
            "fps": self.cfg.video.fps,
            "capture_packets": packets,
            "capture_errors": errors,
            "last_error": self.sink.last_error(),
            "uptime_ms": u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
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
        let session = self
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(s) = session {
            s.stop();
        }
    }
}

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
                    "save_ms": clip.save_ms,
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
            save_from(&sink, dir.path(), 1_000_000_000),
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
        let a = save_from(&sink, dir.path(), 2_000_000_000).unwrap();
        let b = save_from(&sink, dir.path(), 2_000_000_000).unwrap();
        assert_ne!(a.path, b.path);
        assert!(a.path.exists() && b.path.exists());
        assert_eq!(a.frames, 121, "from keyframe 180 to frame 300");
        assert_eq!(std::fs::metadata(&a.path).unwrap().len(), a.bytes);
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
