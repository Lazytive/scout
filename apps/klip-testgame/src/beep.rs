//! Test beeps: the 1 kHz tone of `klip_testkit::audio` starting at every whole second of
//! game time, the moments the white flash frames are scheduled. Each beep is placed using
//! cpal's predicted playback time for the samples it writes, and its game time is logged
//! (`index,nanoseconds`) so tests can measure A/V sync against it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use klip_testkit::audio::beep_sample;

use crate::tap::{self, Event};

/// Keeps the output stream alive and collects beep times.
pub struct Beeper {
    _stream: cpal::Stream,
    log: Arc<Mutex<Vec<(u32, u64)>>>,
    tap: Option<tap::Tap>,
    #[cfg(not(target_os = "linux"))]
    anchor: Arc<OnceLock<(u128, u128)>>,
    /// (performance clock ns, game ns) of one moment (Linux; cpal's clock there starts at
    /// the stream).
    #[cfg(target_os = "linux")]
    clock_anchor: (u128, u128),
    pub rate: u32,
    pub underruns: Arc<AtomicU64>,
}

impl Beeper {
    /// (performance-clock ns, game ns) of the same moment: maps game time to the clock
    /// capture timestamps use (QPC on Windows, where cpal's clock is QPC too;
    /// CLOCK_MONOTONIC on Linux).
    pub fn anchor(&self) -> Option<(u128, u128)> {
        #[cfg(target_os = "linux")]
        return Some(self.clock_anchor);
        #[cfg(not(target_os = "linux"))]
        self.anchor.get().copied()
    }

    /// Beeps that started since the last call: (index, predicted game time in ns).
    pub fn take_log(&self) -> Vec<(u32, u64)> {
        std::mem::take(&mut *self.log.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Beeps measured at the loopback tap since the last call: (index, game time in ns).
    pub fn take_tap_log(&self) -> Vec<(u32, u64)> {
        match &self.tap {
            Some(t) => std::mem::take(&mut *t.log.lock().unwrap_or_else(|e| e.into_inner())),
            None => Vec::new(),
        }
    }
}

/// Callback state.
struct Gen {
    rate: u32,
    channels: usize,
    /// (cpal instant, game time) of the same moment, both in ns.
    anchor: Arc<OnceLock<(u128, u128)>>,
    next_beep: u32,
    pos: Option<u64>,
    log: Arc<Mutex<Vec<(u32, u64)>>>,
    tap: Option<Sender<Event>>,
}

impl Gen {
    fn fill(&mut self, data: &mut [f32], info: &cpal::OutputCallbackInfo) {
        let Some(&(anchor_cpal, anchor_game)) = self.anchor.get() else {
            data.fill(0.0);
            return;
        };
        let playback = info.timestamp().playback.as_nanos();
        // Game time (ns) at which the first frame of this buffer will play.
        let first = anchor_game as i128 + (playback as i128 - anchor_cpal as i128);
        for (i, frame) in data.chunks_mut(self.channels).enumerate() {
            let t = first + i as i128 * 1_000_000_000 / i128::from(self.rate);
            let v = match self.pos {
                Some(n) => {
                    let v = beep_sample(n, self.rate);
                    self.pos = v.map(|_| n + 1);
                    v.unwrap_or(0.0)
                }
                None if t >= i128::from(self.next_beep) * 1_000_000_000 => {
                    self.log
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((self.next_beep, t.max(0) as u64));
                    if let Some(tx) = &self.tap {
                        let _ = tx.send(Event::Scheduled(self.next_beep, t));
                    }
                    self.next_beep += 1;
                    self.pos = Some(1);
                    beep_sample(0, self.rate).unwrap_or(0.0)
                }
                None => 0.0,
            };
            frame.fill(v as f32);
        }
    }
}

/// Open the default output device and start beeping on the game clock that began at
/// `game_start`. With `measure`, also time each beep at the loopback tap (`tap.rs`).
pub fn start(game_start: Instant, measure: bool) -> Result<Beeper> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no audio output device"))?;
    let supported = device
        .default_output_config()
        .context("audio output config")?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        bail!("audio output is not f32 ({:?})", supported.sample_format());
    }
    let config = supported.config();
    let rate = config.sample_rate;
    let log = Arc::new(Mutex::new(Vec::new()));
    let anchor = Arc::new(OnceLock::new());
    let underruns = Arc::new(AtomicU64::new(0));
    let events = measure.then(channel::<Event>);
    let mut generator = Gen {
        rate,
        channels: usize::from(config.channels).max(1),
        anchor: anchor.clone(),
        next_beep: 1,
        pos: None,
        log: log.clone(),
        tap: events.as_ref().map(|(tx, _)| tx.clone()),
    };
    let errors = underruns.clone();
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, info| generator.fill(data, info),
            move |e| {
                errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("audio stream error: {e}");
            },
            None,
        )
        .context("audio output stream")?;
    // Same moment on both clocks (both are QPC on Windows; read back to back).
    let _ = anchor.set((stream.now().as_nanos(), game_start.elapsed().as_nanos()));
    #[cfg(target_os = "linux")]
    let clock_anchor = {
        let mono = klip_platform::qpc_now_ns();
        let game = game_start.elapsed().as_nanos();
        (u128::try_from(mono).unwrap_or(0), game)
    };
    #[cfg(target_os = "linux")]
    let tap = match events {
        Some(ev) => {
            let zero = i128::try_from(clock_anchor.0).unwrap_or(0)
                - i128::try_from(clock_anchor.1).unwrap_or(0);
            Some(tap::start_pipewire(
                rate,
                i64::try_from(zero).unwrap_or(0),
                ev,
            )?)
        }
        None => None,
    };
    #[cfg(not(target_os = "linux"))]
    let tap = match events {
        Some(ev) => Some(tap::start(&device, anchor.clone(), ev)?),
        None => None,
    };
    stream.play().context("start audio")?;
    Ok(Beeper {
        _stream: stream,
        log,
        tap,
        #[cfg(not(target_os = "linux"))]
        anchor,
        #[cfg(target_os = "linux")]
        clock_anchor,
        rate,
        underruns,
    })
}
