//! Measures when each beep actually passes the audio engine: a WASAPI loopback capture
//! of the default output device (cpal input stream on an output device), whose packets
//! carry WASAPI's own QPC capture position. A worker thread finds each beep's onset by
//! normalised cross-correlation with the known 1 kHz waveform, which works even with other
//! sound playing, and logs it on the game clock. This is the reference the A/V sync test
//! compares against (cpal's predicted playback time is only an estimate).

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, StreamTrait};
use klip_testkit::audio::beep_sample;

/// Template length: two periods of the 1 kHz beep.
const TEMPLATE_MS: u32 = 2;
/// Search window around the predicted onset.
const BEFORE_NS: i128 = 80_000_000;
const AFTER_NS: i128 = 200_000_000;
/// Gap between packets that counts as a discontinuity.
const GAP_NS: i128 = 2_000_000;

pub enum Event {
    /// A beep was scheduled at this game time (ns).
    Scheduled(u32, i128),
    /// Captured mono samples; the first was captured at this game time (ns).
    Audio(i128, Vec<f32>),
}

pub struct Tap {
    _stream: cpal::Stream,
    pub log: Arc<Mutex<Vec<(u32, u64)>>>,
}

/// Start the loopback capture and the detector. `anchor` maps cpal instants to game time.
pub fn start(
    device: &cpal::Device,
    anchor: Arc<OnceLock<(u128, u128)>>,
    events: (Sender<Event>, Receiver<Event>),
) -> Result<Tap> {
    let supported = device.default_output_config().context("loopback config")?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        bail!("loopback is not f32");
    }
    let config = supported.config();
    let rate = config.sample_rate;
    let channels = usize::from(config.channels).max(1);
    let (tx, rx) = events;
    let stream = device
        .build_input_stream::<f32, _, _>(
            config,
            move |data, info| {
                let Some(&(anchor_cpal, anchor_game)) = anchor.get() else {
                    return;
                };
                let capture = info.timestamp().capture.as_nanos();
                let t = anchor_game as i128 + (capture as i128 - anchor_cpal as i128);
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|f| f.iter().sum::<f32>() / channels as f32)
                    .collect();
                let _ = tx.send(Event::Audio(t, mono));
            },
            |e| eprintln!("loopback error: {e}"),
            None,
        )
        .context("loopback stream")?;
    stream.play().context("start loopback")?;
    let log = Arc::new(Mutex::new(Vec::new()));
    let out = log.clone();
    std::thread::Builder::new()
        .name("beep-tap".into())
        .spawn(move || {
            let mut d = Detector::new(rate);
            while let Ok(ev) = rx.recv() {
                for found in d.feed(ev) {
                    out.lock().unwrap_or_else(|e| e.into_inner()).push(found);
                }
            }
        })
        .context("tap thread")?;
    Ok(Tap {
        _stream: stream,
        log,
    })
}

struct Detector {
    rate: u32,
    template: Vec<f32>,
    template_norm: f32,
    buf: VecDeque<f32>,
    buf_start: i128,
    pending: VecDeque<(u32, i128)>,
}

impl Detector {
    fn new(rate: u32) -> Detector {
        let n = (rate * TEMPLATE_MS / 1000) as u64;
        let template: Vec<f32> = (0..n)
            .map(|i| beep_sample(i, rate).unwrap_or(0.0) as f32)
            .collect();
        let template_norm = template.iter().map(|v| v * v).sum::<f32>().sqrt();
        Detector {
            rate,
            template,
            template_norm,
            buf: VecDeque::new(),
            buf_start: 0,
            pending: VecDeque::new(),
        }
    }

    fn ns(&self, samples: usize) -> i128 {
        samples as i128 * 1_000_000_000 / i128::from(self.rate)
    }

    fn index(&self, t: i128) -> usize {
        ((t - self.buf_start).max(0) * i128::from(self.rate) / 1_000_000_000) as usize
    }

    fn feed(&mut self, ev: Event) -> Vec<(u32, u64)> {
        match ev {
            Event::Scheduled(k, t) => {
                self.pending.push_back((k, t));
                Vec::new()
            }
            Event::Audio(t, samples) => {
                let expected = self.buf_start + self.ns(self.buf.len());
                if self.buf.is_empty() || (t - expected).abs() > GAP_NS {
                    self.buf.clear();
                    self.buf_start = t;
                }
                self.buf.extend(samples);
                let max = self.rate as usize * 2;
                if self.buf.len() > max {
                    let drop = self.buf.len() - max;
                    self.buf.drain(..drop);
                    self.buf_start += self.ns(drop);
                }
                self.detect()
            }
        }
    }

    fn detect(&mut self) -> Vec<(u32, u64)> {
        let mut found = Vec::new();
        let end = self.buf_start + self.ns(self.buf.len());
        while let Some(&(k, est)) = self.pending.front() {
            if end < est + AFTER_NS {
                break;
            }
            self.pending.pop_front();
            if est - BEFORE_NS < self.buf_start {
                continue;
            }
            if let Some(i) = self.onset(self.index(est - BEFORE_NS), self.index(est + AFTER_NS)) {
                let t = self.buf_start + self.ns(i);
                found.push((k, t.max(0) as u64));
            }
        }
        found
    }

    /// First local maximum of the normalised correlation within 95 % of the best score.
    fn onset(&self, from: usize, to: usize) -> Option<usize> {
        let x: Vec<f32> = self
            .buf
            .iter()
            .skip(from)
            .take(to.saturating_sub(from))
            .copied()
            .collect();
        let scores: Vec<f32> = x
            .windows(self.template.len())
            .map(|w| {
                let dot: f32 = w.iter().zip(&self.template).map(|(a, b)| a * b).sum();
                let norm = w.iter().map(|v| v * v).sum::<f32>().sqrt();
                dot / (norm * self.template_norm + 1e-9)
            })
            .collect();
        let best = scores.iter().copied().fold(0.0f32, f32::max);
        if best < 0.5 {
            return None;
        }
        scores
            .windows(3)
            .position(|w| matches!(w, [p, c, n] if *c >= 0.95 * best && c >= p && c >= n))
            .map(|i| from + i + 1)
    }
}
