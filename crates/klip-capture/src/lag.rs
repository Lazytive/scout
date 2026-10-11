//! How late per-process loopback timestamps are, measured against the system mix.
//!
//! WASAPI process loopback hands out regular packets, but stamps them a constant 13–19 ms
//! later than the system loopback stamps the same sound (it differs per session; measured
//! with `klip-platform/tests/loopback_offset.rs`). System loopback stamps are exact (within
//! 1 ms of the audio engine's own timing). So while a process is recorded, the system mix
//! is captured too, purely as a timing reference (kept in memory for a few seconds, never
//! written), and [`LagEstimator`] finds the lag by cross-correlating the loudness
//! envelopes of the two streams. Envelopes, not raw samples: a pure tone correlates
//! equally well at every period of the tone.
//!
//! Linux (PipeWire): an app's stream, linked straight to Scout's capture, arrives one graph
//! cycle *before* the output's mix carries it (measured −10.5 to −10.75 ms with a
//! 512-frame quantum). The capture already stamps such streams one buffer later; the
//! estimator corrects what remains (either sign).

use std::collections::VecDeque;

/// Envelope sample rate.
const ENV_RATE: i64 = 8_000;
const ENV_STEP_NS: i64 = 1_000_000_000 / ENV_RATE;
/// Input samples averaged into one envelope sample (48 kHz → 8 kHz).
const DECIMATE: usize = 6;
/// Envelope history kept per stream.
const KEEP: usize = 3 * ENV_RATE as usize;
/// Correlation window.
const WINDOW: usize = 3 * ENV_RATE as usize / 2;
/// Lags searched: the process stream between 50 ms early (Linux: up to two 1024-frame
/// graph cycles) and 60 ms late.
const MIN_LAG: i64 = -50 * ENV_RATE / 1000;
const MAX_LAG: i64 = 60 * ENV_RATE / 1000;
/// How often a new estimate is attempted.
const EVERY_NS: i64 = 2_000_000_000;
/// Weakest normalised correlation accepted as a measurement.
const MIN_SCORE: f64 = 0.6;
/// Estimates kept for the median.
const HISTORY: usize = 5;

/// Lag assumed until the first measurement: Windows, the mean of the measured 13–19 ms;
/// Linux, none (the capture shifts app streams by the one graph cycle they are early).
pub const DEFAULT_LAG_NS: i64 = if cfg!(windows) { 16_000_000 } else { 0 };

/// One stream's envelope on a grid of `ENV_STEP_NS`, placed by capture time.
#[derive(Debug, Default)]
struct Envelope {
    /// Time (ns) of `buf[0]`.
    base: Option<i64>,
    buf: VecDeque<f32>,
}

impl Envelope {
    fn push(&mut self, t_ns: i64, mono: &[f32]) {
        let base = *self.base.get_or_insert(t_ns);
        let Some(mut idx) = t_ns
            .checked_sub(base)
            .and_then(|d| d.checked_div(ENV_STEP_NS))
            .and_then(|i| usize::try_from(i).ok())
        else {
            // Before the start of the history: ignore.
            return;
        };
        for chunk in mono.chunks(DECIMATE) {
            let n = chunk.len().max(1) as f32;
            let v = chunk.iter().map(|s| s.abs()).sum::<f32>() / n;
            if idx > self.buf.len().saturating_add(ENV_RATE as usize) {
                // A jump of over a second: start again.
                self.buf.clear();
                self.base = Some(t_ns);
                idx = 0;
            }
            while self.buf.len() <= idx {
                self.buf.push_back(0.0);
            }
            if let Some(slot) = self.buf.get_mut(idx) {
                *slot = v;
            }
            idx = idx.saturating_add(1);
        }
        while self.buf.len() > KEEP {
            self.buf.pop_front();
            self.base = self.base.map(|b| b.saturating_add(ENV_STEP_NS));
        }
    }

    fn end(&self) -> Option<i64> {
        let len = i64::try_from(self.buf.len()).ok()?;
        self.base?.checked_add(len.checked_mul(ENV_STEP_NS)?)
    }

    /// `len` envelope samples ending at `end_ns` (shifted by `lag` samples), smoothed over
    /// 1 ms and with the mean removed.
    fn window(&self, end_ns: i64, len: usize, lag: i64) -> Option<Vec<f64>> {
        let base = self.base?;
        let end_idx = end_ns
            .checked_sub(base)?
            .checked_div(ENV_STEP_NS)?
            .checked_sub(lag)?;
        let start = end_idx.checked_sub(i64::try_from(len).ok()?)?;
        let (start, end) = (usize::try_from(start).ok()?, usize::try_from(end_idx).ok()?);
        if end > self.buf.len() {
            return None;
        }
        let raw: Vec<f64> = self.buf.range(start..end).map(|v| f64::from(*v)).collect();
        // 1 ms moving average.
        let k = (ENV_RATE / 1000) as usize;
        let mut smooth = Vec::with_capacity(raw.len());
        let mut acc = 0.0;
        for (i, v) in raw.iter().enumerate() {
            acc += v;
            if let Some(old) = i.checked_sub(k).and_then(|j| raw.get(j)) {
                acc -= old;
            }
            smooth.push(acc / k as f64);
        }
        let mean = smooth.iter().sum::<f64>() / smooth.len().max(1) as f64;
        Some(smooth.into_iter().map(|v| v - mean).collect())
    }
}

/// See the module documentation.
#[derive(Debug)]
pub struct LagEstimator {
    reference: Envelope,
    delayed: Envelope,
    estimates: VecDeque<i64>,
    current: i64,
    next_try: Option<i64>,
    measurements: u64,
}

impl Default for LagEstimator {
    fn default() -> Self {
        LagEstimator::new()
    }
}

impl LagEstimator {
    pub fn new() -> LagEstimator {
        LagEstimator {
            reference: Envelope::default(),
            delayed: Envelope::default(),
            estimates: VecDeque::new(),
            current: DEFAULT_LAG_NS,
            next_try: None,
            measurements: 0,
        }
    }

    /// How late the process stream's stamps are (ns): the median of recent
    /// measurements, or [`DEFAULT_LAG_NS`] before the first one.
    pub fn lag(&self) -> i64 {
        self.current
    }

    /// Successful measurements so far.
    pub fn measurements(&self) -> u64 {
        self.measurements
    }

    /// Mono 48 kHz audio of the system mix, first sample captured at `t_ns`.
    pub fn push_reference(&mut self, t_ns: i64, mono: &[f32]) {
        self.reference.push(t_ns, mono);
    }

    /// Mono 48 kHz audio of the process stream, stamped `t_ns` by its capture. Every two
    /// seconds this also updates the estimate.
    pub fn push_delayed(&mut self, t_ns: i64, mono: &[f32]) {
        self.delayed.push(t_ns, mono);
        let next = *self.next_try.get_or_insert(t_ns.saturating_add(EVERY_NS));
        if t_ns >= next {
            self.next_try = Some(t_ns.saturating_add(EVERY_NS));
            if let Some(lag) = self.measure() {
                self.measurements = self.measurements.saturating_add(1);
                self.estimates.push_back(lag);
                while self.estimates.len() > HISTORY {
                    self.estimates.pop_front();
                }
                let mut sorted: Vec<i64> = self.estimates.iter().copied().collect();
                sorted.sort_unstable();
                if let Some(m) = sorted.get(sorted.len() / 2) {
                    self.current = *m;
                }
            }
        }
    }

    /// Lag (ns) maximising the envelope correlation, if the match is clear.
    fn measure(&self) -> Option<i64> {
        // Leave room for the latest packets of either stream to arrive.
        let end = self
            .reference
            .end()?
            .min(self.delayed.end()?)
            .checked_sub(100_000_000)?;
        let b = self.delayed.window(end, WINDOW, 0)?;
        let bb: f64 = b.iter().map(|v| v * v).sum();
        if bb < 1e-9 {
            return None; // the process is silent
        }
        let mut best: Option<(i64, f64)> = None;
        for lag in MIN_LAG..=MAX_LAG {
            let Some(a) = self.reference.window(end, WINDOW, lag) else {
                continue;
            };
            let aa: f64 = a.iter().map(|v| v * v).sum();
            if aa < 1e-12 {
                continue;
            }
            let ab: f64 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            let score = ab / (aa * bb).sqrt();
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((lag, score));
            }
        }
        let (lag, score) = best?;
        (score >= MIN_SCORE && lag > MIN_LAG && lag < MAX_LAG)
            .then(|| lag.saturating_mul(ENV_STEP_NS))
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    /// 48 kHz mono test signals: beeps (1 kHz bursts) once a second, or noise bursts.
    fn signal(secs: usize, tone: bool) -> Vec<f32> {
        let mut seed = 12345u32;
        (0..secs * 48_000)
            .map(|i| {
                let in_burst = i % 48_000 < 2_880 || (!tone && i % 12_000 < 600);
                if !in_burst {
                    return 0.0;
                }
                if tone {
                    (0.25 * (std::f64::consts::TAU * 1000.0 * i as f64 / 48_000.0).cos()) as f32
                } else {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                }
            })
            .collect()
    }

    /// Feed 10 ms packets of `sig` as the reference, and the same audio stamped
    /// `delay_ns` later (plus the user's `other` sound only in the reference).
    fn run(sig: &[f32], other: &[f32], delay_ns: i64) -> LagEstimator {
        let mut e = LagEstimator::new();
        let t0 = 7_000_000_000i64;
        for (k, chunk) in sig.chunks(480).enumerate() {
            let t = t0 + k as i64 * 10_000_000;
            let mixed: Vec<f32> = chunk
                .iter()
                .zip(other.iter().skip(k * 480))
                .map(|(a, b)| a + b)
                .collect();
            e.push_reference(t, &mixed);
            e.push_delayed(t + delay_ns, chunk);
        }
        e
    }

    #[test]
    fn finds_the_lag_of_beeps_and_of_noise() {
        for tone in [true, false] {
            for delay in [13_000_000, 16_400_000, 18_500_000] {
                let e = run(&signal(12, tone), &vec![0.0; 12 * 48_000], delay);
                assert!(e.measurements() >= 3, "tone {tone}: {e:?}");
                assert!(
                    (e.lag() - delay).abs() <= 1_000_000,
                    "tone {tone} delay {delay}: got {}",
                    e.lag()
                );
            }
        }
    }

    #[test]
    fn finds_streams_that_are_early() {
        // Linux: an app's stream reaches Scout one graph cycle before the output's mix.
        for delay in [-10_666_667, -21_333_333, -42_666_667] {
            let e = run(&signal(12, false), &vec![0.0; 12 * 48_000], delay);
            assert!(e.measurements() >= 3, "{delay}: {e:?}");
            assert!(
                (e.lag() - delay).abs() <= 1_000_000,
                "{delay}: got {}",
                e.lag()
            );
        }
    }

    #[test]
    fn other_sound_in_the_mix_does_not_fool_it() {
        // Constant music-like noise in the system mix only, as loud as the game.
        let mut seed = 99u32;
        let music: Vec<f32> = (0..12 * 48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.3
            })
            .collect();
        let e = run(&signal(12, false), &music, 15_000_000);
        assert!((e.lag() - 15_000_000).abs() <= 1_000_000, "got {}", e.lag());
    }

    #[test]
    fn silence_keeps_the_default() {
        let e = run(&vec![0.0; 10 * 48_000], &vec![0.0; 10 * 48_000], 15_000_000);
        assert_eq!(e.measurements(), 0);
        assert_eq!(e.lag(), DEFAULT_LAG_NS);
        // Hostile timestamps don't panic.
        let mut e = LagEstimator::new();
        e.push_reference(i64::MAX, &[1.0; 480]);
        e.push_delayed(i64::MIN, &[1.0; 480]);
        e.push_delayed(i64::MAX, &[1.0; 480]);
        e.push_reference(0, &[]);
    }
}
