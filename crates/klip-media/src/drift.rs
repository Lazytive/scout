//! A continuous audio stream that follows its packets' capture times.
//!
//! Capture APIs stamp each packet with the exact time its first frame was captured
//! (WASAPI: QueryPerformanceCounter time). Encoders and muxers want back-to-back samples
//! instead, so [`DriftAligner`] gives every packet the timestamp right after the previous
//! one and keeps that timeline within about a frame of the capture times: when the
//! (smoothed) difference grows past half a frame, one frame is repeated or dropped. That
//! absorbs the audio device's clock drift against the system clock (typically under
//! ±100 ppm, i.e. one frame every 0.2 s at most) without audible jumps. A difference over
//! [`RESYNC`] (a real gap or glitch) restarts the timeline at the capture time.

use crate::{NANOS_PER_SEC, Nanos};

/// Timeline jumps larger than this restart the stream instead of being corrected.
pub const RESYNC: Nanos = 20_000_000;

/// How to emit one packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aligned {
    /// Timestamp of the packet's first frame.
    pub pts: Nanos,
    /// Frames to add (+1: repeat the last frame) or remove (-1: drop the last frame).
    pub adjust: i32,
    /// The timeline restarted here (first packet, or a gap/jump).
    pub discont: bool,
}

/// See the module documentation.
#[derive(Clone, Debug)]
pub struct DriftAligner {
    rate: u32,
    /// Start of the current timeline and frames emitted since.
    base: Nanos,
    emitted: u64,
    started: bool,
    /// Smoothed (capture time - timeline position), ns.
    error: i64,
}

impl DriftAligner {
    /// An aligner for `rate` Hz; `None` for 0.
    pub fn new(rate: u32) -> Option<DriftAligner> {
        (rate > 0).then_some(DriftAligner {
            rate,
            base: 0,
            emitted: 0,
            started: false,
            error: 0,
        })
    }

    fn frames_ns(&self, frames: u64) -> Nanos {
        let ns = i128::from(frames)
            .saturating_mul(i128::from(NANOS_PER_SEC))
            .checked_div(i128::from(self.rate))
            .unwrap_or(0);
        i64::try_from(ns).unwrap_or(Nanos::MAX)
    }

    /// Where the next frame goes on the current timeline.
    pub fn next_pts(&self) -> Option<Nanos> {
        self.started
            .then(|| self.base.saturating_add(self.frames_ns(self.emitted)))
    }

    /// Plan one packet of `frames` frames captured at `capture`.
    pub fn align(&mut self, capture: Nanos, frames: u32) -> Aligned {
        let frame = self.frames_ns(1).max(1);
        let restart = match self.next_pts() {
            Some(next) => capture.saturating_sub(next).saturating_abs() > RESYNC,
            None => true,
        };
        if restart {
            self.base = capture;
            self.emitted = 0;
            self.started = true;
            self.error = 0;
        }
        let pts = self.next_pts().unwrap_or(capture);
        // Smooth over ~16 packets so capture-time jitter doesn't trigger corrections.
        let diff = capture.saturating_sub(pts);
        self.error = self
            .error
            .saturating_add(diff.saturating_sub(self.error).checked_div(16).unwrap_or(0));
        let half = frame.checked_div(2).unwrap_or(0);
        let adjust = if frames == 0 {
            0
        } else if self.error > half {
            // Our timeline runs early: add a frame so later packets land later.
            self.error = self.error.saturating_sub(frame);
            1
        } else if self.error < half.saturating_neg() && frames > 1 {
            self.error = self.error.saturating_add(frame);
            -1
        } else {
            0
        };
        let out = u64::from(frames).saturating_add_signed(i64::from(adjust));
        self.emitted = self.emitted.saturating_add(out);
        Aligned {
            pts,
            adjust,
            discont: restart,
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::unwrap_used)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Feed `secs` of 10 ms packets from a device whose clock is off by `ppm`; return the
    /// largest |pts - capture time| after the first second and the net adjustment.
    fn run(ppm: i64, jitter: i64, secs: i64) -> (i64, i64) {
        let mut a = DriftAligner::new(RATE).unwrap();
        let mut worst = 0;
        let mut net = 0i64;
        for k in 0..secs * 100 {
            // Packet k holds frames k*480.. ; on a device clock off by ppm.
            let ideal = k * 10_000_000;
            // Zero-mean jitter; the first packet (the timeline origin) has none.
            let j = match k {
                0 => 0,
                k if k % 2 == 0 => jitter,
                _ => -jitter,
            };
            let capture = 5_000_000_000 + ideal + ideal * ppm / 1_000_000 + j;
            let al = a.align(capture, 480);
            assert!(!al.discont || k == 0, "unexpected restart at {k}");
            net += i64::from(al.adjust);
            if k >= 100 {
                worst = worst.max((al.pts - capture).abs());
            }
        }
        (worst, net)
    }

    #[test]
    fn follows_drift_within_a_frame() {
        for ppm in [-100, -20, 0, 20, 100] {
            let (worst, net) = run(ppm, 50_000, 60);
            // 60 s at `ppm` = 0.06 * ppm ms = that many frames at 48 kHz (~21 µs each).
            let expected = 60 * 48_000 * ppm / 1_000_000;
            assert!(
                (net - expected).abs() <= 2,
                "{ppm} ppm: net {net}, expected {expected}"
            );
            assert!(worst < 100_000, "{ppm} ppm: worst error {worst} ns");
        }
    }

    #[test]
    fn jitter_alone_causes_no_corrections() {
        let (_, net) = run(0, 200_000, 30);
        assert_eq!(net, 0);
    }

    #[test]
    fn gaps_restart_the_timeline() {
        let mut a = DriftAligner::new(RATE).unwrap();
        assert!(a.align(1_000_000_000, 480).discont);
        let b = a.align(1_010_000_000, 480);
        assert_eq!((b.pts, b.discont), (1_010_000_000, false));
        // 100 ms of nothing, then data: restart at the capture time.
        let c = a.align(1_120_000_000, 480);
        assert_eq!((c.pts, c.discont), (1_120_000_000, true));
        // Going back in time also restarts; hostile values don't panic.
        assert!(a.align(0, 480).discont);
        let _ = a.align(Nanos::MAX, u32::MAX);
        let _ = a.align(Nanos::MIN, 0);
        assert!(DriftAligner::new(0).is_none());
    }
}
