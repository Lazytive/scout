//! Commands for the settings page and the first-run setup: encoder detection, audio
//! devices, the microphone meter, the buffer RAM estimate, pausing hotkeys while a new
//! one is typed, and programs to add as games.

use std::time::{Duration, Instant};

use klip_control::CommandError;
use klip_media::{Codec, NANOS_PER_MILLI};
use klip_platform::{AudioCapture, AudioCaptureSink, AudioPacket, CaptureEndpoint};
use klip_settings::{DesktopAudio, Settings};
use serde_json::{Value, json};

use super::{Inner, lock};
use crate::config;
use crate::events::Event;
use crate::games;

/// The meter stops by itself after this long (a settings page that went away).
pub(super) const METER_MAX: Duration = Duration::from_secs(120);
/// Hotkeys come back by themselves after this long.
pub(super) const PAUSE_MAX: Duration = Duration::from_secs(60);
/// Meter events per second, at most.
const METER_EVERY: Duration = Duration::from_millis(66);

fn failed(e: impl std::fmt::Display) -> CommandError {
    CommandError::new("failed", e.to_string())
}

/// dBFS of a linear level (silence: -100).
fn db(level: f32) -> f32 {
    if level <= 0.000_01 {
        -100.0
    } else {
        20.0 * level.log10()
    }
}

/// Peak and RMS of the captured input, reported through `emit` ~15 times a second.
struct MeterSink {
    peak: f32,
    sum_sq: f64,
    samples: u64,
    last: Instant,
    emit: Box<dyn Fn(f32, f32) + Send>,
}

impl AudioCaptureSink for MeterSink {
    fn packet(&mut self, p: AudioPacket<'_>) {
        if let Some(data) = p.data {
            for s in data {
                self.peak = self.peak.max(s.abs());
                self.sum_sq += f64::from(*s) * f64::from(*s);
            }
            self.samples = self
                .samples
                .saturating_add(u64::try_from(data.len()).unwrap_or(0));
        } else {
            // Silence: counts for the RMS.
            self.samples = self
                .samples
                .saturating_add(u64::from(p.frames).saturating_mul(2));
        }
        if self.last.elapsed() >= METER_EVERY && self.samples > 0 {
            #[allow(clippy::cast_precision_loss)] // sample counts are far below 2^52
            let rms = (self.sum_sq / self.samples as f64).sqrt();
            #[allow(clippy::cast_possible_truncation)] // a level in 0..=1
            (self.emit)(db(self.peak), db(rms as f32));
            self.peak = 0.0;
            self.sum_sq = 0.0;
            self.samples = 0;
            self.last = Instant::now();
        }
    }

    fn notice(&mut self, message: String) {
        tracing::info!(%message, "microphone meter");
    }
}

/// Audio tracks a recording with these settings writes (the mix comes first when there
/// is more than one source and separate tracks are on).
pub fn audio_tracks(s: &Settings) -> u32 {
    let a = &s.audio;
    let inputs = u32::from(a.desktop != DesktopAudio::Off)
        .saturating_add(u32::from(a.voice && !a.voice_apps.is_empty()))
        .saturating_add(u32::from(a.mic));
    match inputs {
        0 | 1 => inputs,
        n if a.separate_tracks => n.saturating_add(1),
        _ => 1,
    }
}

impl Inner {
    /// GPUs and the working encoders for a codec, best first (probes them if this
    /// machine's results aren't cached yet: up to a few seconds per encoder).
    pub(super) fn encoders_list(&self, args: &Value) -> Result<Value, CommandError> {
        let codec = match args.get("codec").and_then(Value::as_str) {
            Some(name) => Codec::from_name(name)
                .filter(|c| matches!(c, Codec::H264 | Codec::H265 | Codec::Av1))
                .ok_or_else(|| CommandError::bad_args("codec must be h264, h265 or av1"))?,
            None => Codec::H264,
        };
        let gpus: Vec<Value> = shown_gpus(klip_platform::list_gpus().unwrap_or_default())
            .into_iter()
            .map(|g| {
                json!({
                    "name": g.name,
                    "vendor": g.vendor.name(),
                    "vram_bytes": g.dedicated_video_memory,
                })
            })
            .collect();
        // `fresh` (the Debug self-test): forget this codec's saved probe results first,
        // and report every candidate's outcome, failures included.
        let fresh = args.get("fresh").and_then(Value::as_bool).unwrap_or(false);
        let names: Vec<String> = klip_probe::candidates(codec, &config::gpu_vendors(), None)
            .into_iter()
            .map(|c| c.element)
            .collect();
        let started = Instant::now();
        let list = {
            let mut c = lock(&self.cache);
            if fresh {
                c.0.results.retain(|r| !names.contains(&r.element));
            }
            let r = config::pick_encoders(codec, None, None, &mut c.0);
            config::save_encoder_cache(&c.0, c.1.as_deref());
            r
        };
        if fresh {
            // Ranking stops at the first encoder that works; the self-test wants them all.
            let untested: Vec<klip_probe::EncoderChoice> = {
                let c = lock(&self.cache);
                klip_probe::candidates(codec, &config::gpu_vendors(), None)
                    .into_iter()
                    .filter(|e| c.0.get(&e.element).is_none())
                    .collect()
            };
            let prober = config::prober();
            for choice in untested {
                let r = prober.probe(&choice, Duration::from_secs(10));
                tracing::info!(element = %choice.element, ok = r.ok, "encoder probe (self-test)");
                let mut c = lock(&self.cache);
                c.0.record(&r);
                config::save_encoder_cache(&c.0, c.1.as_deref());
            }
        }
        let tested: Vec<Value> = {
            let c = lock(&self.cache);
            names
                .iter()
                .map(|n| match c.0.get(n) {
                    Some(r) => json!({
                        "element": n, "ok": r.ok, "error": r.error, "probe_ms": r.probe_ms,
                    }),
                    None => json!({ "element": n, "ok": null, "error": "not tested" }),
                })
                .collect()
        };
        let took_ms = started.elapsed().as_millis();
        Ok(match list {
            Ok(l) => json!({
                "tested": tested,
                "gpus": gpus,
                "codec": codec.name(),
                "encoders": l.iter().map(|e| json!({
                    "element": e.element,
                    "description": e.description,
                    "gpu": e.gpu.map(|g| g.name()),
                })).collect::<Vec<_>>(),
                "chosen": l.first().map(|e| e.element.clone()),
                "took_ms": took_ms,
            }),
            Err(e) => json!({
                "tested": tested,
                "gpus": gpus,
                "codec": codec.name(),
                "encoders": [],
                "chosen": null,
                "error": e.to_string(),
                "took_ms": took_ms,
            }),
        })
    }

    pub(super) fn audio_devices(&self) -> Result<Value, CommandError> {
        let devices = klip_capture::audio::list_devices().map_err(failed)?;
        let pick = |loopback: bool| {
            devices
                .iter()
                .filter(|d| d.loopback == loopback)
                .map(|d| json!({ "id": d.id, "name": d.name }))
                .collect::<Vec<_>>()
        };
        Ok(json!({ "inputs": pick(false), "outputs": pick(true) }))
    }

    /// `{"on": true, "device": null|"id"}` starts the microphone meter (`mic_level`
    /// events); `{"on": false}` stops it. It stops by itself after two minutes.
    pub(super) fn audio_meter(&self, args: &Value) -> Result<Value, CommandError> {
        let on = args
            .get("on")
            .and_then(Value::as_bool)
            .ok_or_else(|| CommandError::bad_args("on must be true or false"))?;
        let old = lock(&self.meter).take();
        if let Some((capture, _)) = old {
            capture.stop();
        }
        if !on {
            return Ok(json!({ "on": false }));
        }
        let device = match args.get("device") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.len() <= 4096 => Some(s.clone()),
            Some(_) => return Err(CommandError::bad_args("device must be a device id or null")),
        };
        let me = self.me.clone();
        let sink = MeterSink {
            peak: 0.0,
            sum_sq: 0.0,
            samples: 0,
            last: Instant::now(),
            emit: Box::new(move |peak_db, rms_db| {
                if let Some(inner) = me.upgrade() {
                    inner.emit(&Event::MicLevel { peak_db, rms_db });
                }
            }),
        };
        let capture =
            AudioCapture::start(CaptureEndpoint::Input { device }, 48_000, 2, Box::new(sink))
                .map_err(failed)?;
        *lock(&self.meter) = Some((capture, Instant::now()));
        Ok(json!({ "on": true }))
    }

    /// Stop a meter that ran too long (called by the watcher).
    pub(super) fn meter_watchdog(&self) {
        let expired = lock(&self.meter)
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() >= METER_MAX);
        if expired && let Some((capture, _)) = lock(&self.meter).take() {
            capture.stop();
        }
    }

    /// What the replay buffer would hold in memory with these (draft) settings.
    pub(super) fn recording_estimate(&self, args: &Value) -> Result<Value, CommandError> {
        let s = match args {
            Value::Null => self.settings(),
            other => Settings::from_json_lenient(&other.to_string()).0,
        };
        let eff = s.effective(None);
        let refresh = config::target_refresh(klip_capture::VideoTarget::PrimaryMonitor);
        let q = eff.quality.params(refresh);
        let tracks = audio_tracks(&s);
        let audio_kbps = s.audio.bitrate_kbps.saturating_mul(tracks);
        let buffer = i64::from(eff.buffer_secs).saturating_mul(1000 * NANOS_PER_MILLI);
        let bytes =
            klip_recorder::estimate_buffer_bytes(buffer, q.bitrate_kbps.saturating_add(audio_kbps));
        Ok(json!({
            "bytes": bytes,
            "buffer_secs": eff.buffer_secs,
            "video_kbps": q.bitrate_kbps,
            "audio_kbps": audio_kbps,
            "audio_tracks": tracks,
            "fps": q.fps,
            "max_width": q.max_width,
            "max_height": q.max_height,
            "codec": q.codec,
        }))
    }

    /// Unbind the hotkeys while the user types a new one (else pressing an existing
    /// combination would run it instead of reaching the window). Back after a minute.
    pub(super) fn hotkeys_pause(&self) -> Value {
        let old = lock(&self.shortcuts).take();
        drop(old);
        *lock(&self.hotkeys_paused) = Some(Instant::now());
        json!({ "paused": true, "resume_after_secs": PAUSE_MAX.as_secs() })
    }

    pub(super) fn hotkeys_resume(&self) -> Value {
        let was = lock(&self.hotkeys_paused).take().is_some();
        if was {
            self.bind_hotkeys();
        }
        json!({ "paused": false })
    }

    /// Resume hotkeys a settings page forgot to resume (called by the watcher).
    pub(super) fn hotkeys_watchdog(&self) {
        let expired = lock(&self.hotkeys_paused).is_some_and(|since| since.elapsed() >= PAUSE_MAX);
        if expired {
            self.hotkeys_resume();
        }
    }

    pub(super) fn games_candidates(&self) -> Result<Value, CommandError> {
        let settings = self.settings();
        let snap = games::snapshot().map_err(failed)?;
        Ok(json!({ "programs": games::candidates(&snap, &settings.games) }))
    }
}

/// The GPUs to show: hardware adapters, each model once. Windows can list one GPU as two
/// adapters with different LUIDs (an RX 9070 XT on the AMD tester's PC, 2026-10-12); two
/// identical cards show once too, which costs nothing (the encoders list names both).
fn shown_gpus(all: Vec<klip_platform::GpuAdapter>) -> Vec<klip_platform::GpuAdapter> {
    let mut out: Vec<klip_platform::GpuAdapter> = Vec::new();
    for g in all.into_iter().filter(|g| !g.software) {
        if !out
            .iter()
            .any(|o| o.vendor_id == g.vendor_id && o.device_id == g.device_id && o.name == g.name)
        {
            out.push(g);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(name: &str, device_id: u32, luid: i64, software: bool) -> klip_platform::GpuAdapter {
        klip_platform::GpuAdapter {
            index: 0,
            name: name.into(),
            vendor: klip_platform::GpuVendor::from_pci_id(0x1002),
            vendor_id: 0x1002,
            device_id,
            luid,
            dedicated_video_memory: 16 << 30,
            software,
            driver_version: None,
        }
    }

    #[test]
    fn one_gpu_listed_as_two_adapters_shows_once() {
        let shown = shown_gpus(vec![
            gpu("AMD Radeon RX 9070 XT", 0x7550, 1, false),
            gpu("AMD Radeon(TM) Graphics", 0x13c0, 2, false),
            gpu("AMD Radeon RX 9070 XT", 0x7550, 3, false),
            gpu("Microsoft Basic Render Driver", 0x8c, 4, true),
        ]);
        let names: Vec<&str> = shown.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["AMD Radeon RX 9070 XT", "AMD Radeon(TM) Graphics"]);
    }

    #[test]
    fn track_counts_follow_the_layout() {
        let mut s = Settings::default();
        s.audio.voice_apps.clear();
        assert_eq!(audio_tracks(&s), 1, "game only");
        s.audio.mic = true;
        assert_eq!(audio_tracks(&s), 3, "mix + game + mic");
        s.audio.separate_tracks = false;
        assert_eq!(audio_tracks(&s), 1, "mix only");
        s.audio.desktop = DesktopAudio::Off;
        s.audio.mic = false;
        assert_eq!(audio_tracks(&s), 0);
    }

    #[test]
    fn levels_in_db() {
        assert!((db(1.0) - 0.0).abs() < 1e-6);
        assert!((db(0.5) + 6.0206).abs() < 1e-3);
        assert!((db(0.0) + 100.0).abs() < 1e-6);
    }
}
