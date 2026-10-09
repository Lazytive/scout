//! Snapping capture timestamps onto a constant-frame-rate grid.
//!
//! Windows Graphics Capture stamps frames when they arrive, so intervals jitter by a
//! couple of milliseconds around the nominal period. Editors and phones prefer constant
//! frame rate, so [`FrameGrid`] moves each timestamp to the nearest slot of a grid that
//! starts at the first frame. Every frame is kept: a frame that lands on an occupied
//! slot takes the next one, and a missing frame leaves an empty slot (a longer sample in
//! the MP4). The error is at most half a period, or up to 1.25 periods right after a burst.
//! If the source runs persistently faster than the grid, the grid restarts at the real
//! timestamp instead of drifting ahead of real time.
//!
//! The first frame can arrive at any moment (capture starts mid-cycle), so the grid
//! re-centres every [`RECENTER_FRAMES`] frames on the average arrival phase. Without that,
//! every frame after it would carry the first frame's phase error (up to half a frame),
//! which differs per recording and shows up as A/V offset.

use crate::{NANOS_PER_SEC, Nanos};

/// Frames averaged before the grid re-centres on the arrival phase.
pub const RECENTER_FRAMES: u32 = 30;

/// Smallest average phase error worth moving the grid for.
const RECENTER_MIN: Nanos = 500_000;

/// Maps frame timestamps to a fixed grid of `fps` slots per second.
#[derive(Clone, Debug)]
pub struct FrameGrid {
    fps: u32,
    origin: Option<Nanos>,
    last_slot: i64,
    last_out: Nanos,
    restarts: u64,
    /// Sum and count of (timestamp - nearest slot) since the last re-centre.
    phase_sum: i64,
    phase_n: u32,
}

impl FrameGrid {
    /// A grid for `fps` frames per second; `None` for 0.
    pub fn new(fps: u32) -> Option<FrameGrid> {
        (fps > 0).then_some(FrameGrid {
            fps,
            origin: None,
            last_slot: 0,
            last_out: 0,
            restarts: 0,
            phase_sum: 0,
            phase_n: 0,
        })
    }

    /// One frame period in nanoseconds (rounded down).
    pub fn period(&self) -> Nanos {
        NANOS_PER_SEC
            .checked_div(i64::from(self.fps))
            .unwrap_or(NANOS_PER_SEC)
    }

    /// How often the grid had to restart (source faster than the grid or time jumps).
    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// Time of `slot` relative to the origin: `slot * 1e9 / fps`, rounded to the nearest ns.
    fn slot_offset(&self, slot: i64) -> Option<Nanos> {
        let num = i128::from(slot).checked_mul(i128::from(NANOS_PER_SEC))?;
        let fps = i128::from(self.fps);
        let q = num.checked_add(fps.checked_div(2)?)?.checked_div(fps)?;
        i64::try_from(q).ok()
    }

    /// Nearest slot to `rel` nanoseconds after the origin.
    fn nearest_slot(&self, rel: Nanos) -> Option<i64> {
        let num = i128::from(rel).checked_mul(i128::from(self.fps))?;
        let den = i128::from(NANOS_PER_SEC);
        let q = num.checked_add(den.checked_div(2)?)?.checked_div(den)?;
        i64::try_from(q).ok()
    }

    fn restart(&mut self, pts: Nanos) -> Nanos {
        // Never go backwards or reuse the previous timestamp.
        let min = self
            .last_out
            .saturating_add(self.period().checked_div(4).unwrap_or(1).max(1));
        let out = if self.origin.is_some() {
            pts.max(min)
        } else {
            pts
        };
        self.origin = Some(out);
        self.last_slot = 0;
        self.last_out = out;
        self.phase_sum = 0;
        self.phase_n = 0;
        out
    }

    /// Track the phase of a frame that landed on its own nearest slot; every
    /// [`RECENTER_FRAMES`] such frames, move the origin by their average phase.
    fn track_phase(&mut self, phase: Nanos) {
        self.phase_sum = self.phase_sum.saturating_add(phase);
        self.phase_n = self.phase_n.saturating_add(1);
        if self.phase_n < RECENTER_FRAMES {
            return;
        }
        let mean = self
            .phase_sum
            .checked_div(i64::from(self.phase_n))
            .unwrap_or(0);
        self.phase_sum = 0;
        self.phase_n = 0;
        if mean.saturating_abs() >= RECENTER_MIN
            && let Some(origin) = self.origin
        {
            // |mean| <= half a period, so the next slot still lands after `last_out`.
            self.origin = Some(origin.saturating_add(mean));
        }
    }

    /// Snap one timestamp. Outputs are strictly increasing.
    pub fn snap(&mut self, pts: Nanos) -> Nanos {
        let Some(origin) = self.origin else {
            return self.restart(pts);
        };
        let period = self.period();
        let slot = pts
            .checked_sub(origin)
            .filter(|rel| *rel >= 0)
            .and_then(|rel| self.nearest_slot(rel));
        let Some(slot) = slot else {
            self.restarts = self.restarts.saturating_add(1);
            return self.restart(pts);
        };
        let bumped = slot <= self.last_slot;
        let slot = if bumped {
            self.last_slot.saturating_add(1)
        } else {
            slot
        };
        let out = self.slot_offset(slot).and_then(|o| origin.checked_add(o));
        match out {
            // Running more than a period ahead of real time: the source is faster than
            // the grid. Restart at the real timestamp rather than drift.
            Some(out) if out.saturating_sub(pts) <= period => {
                self.last_slot = slot;
                self.last_out = out;
                if !bumped {
                    self.track_phase(pts.saturating_sub(out));
                }
                out
            }
            _ => {
                self.restarts = self.restarts.saturating_add(1);
                self.restart(pts)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::unwrap_used)]
mod tests {
    use super::*;

    const P60: Nanos = 16_666_667;

    #[test]
    fn jittered_frames_land_on_the_grid() {
        let mut g = FrameGrid::new(60).unwrap();
        let base = 5_000_000_000;
        // Zero-mean jitter: the grid stays where the first frame put it.
        let jitter = [0, 2_000_000, -2_000_000, 1_000_000, -1_000_000, 0];
        let out: Vec<Nanos> = (0..600)
            .map(|i| g.snap(base + i * P60 + jitter[(i % 6) as usize]))
            .collect();
        for (i, t) in out.iter().enumerate() {
            let want = base + (i as i64 * NANOS_PER_SEC + 30) / 60;
            assert!((t - want).abs() <= 1, "frame {i}: {t} vs {want}");
        }
        assert_eq!(g.restarts(), 0);
    }

    #[test]
    fn grid_recentres_on_the_arrival_phase() {
        let mut g = FrameGrid::new(60).unwrap();
        // The first frame came 6 ms "early" relative to all later ones.
        let pts = |k: i64| if k == 0 { 0 } else { k * P60 + 6_000_000 };
        let mut last = Nanos::MIN;
        for k in 0..200 {
            let t = g.snap(pts(k));
            assert!(t > last);
            if k > 2 * i64::from(RECENTER_FRAMES) {
                assert!((t - pts(k)).abs() < 600_000, "frame {k}: {t} vs {}", pts(k));
                assert!(
                    (t - last - P60).abs() <= 1,
                    "constant rate after re-centring"
                );
            }
            last = t;
        }
        assert_eq!(g.restarts(), 0);
    }

    #[test]
    fn missing_frames_leave_gaps_and_bursts_take_next_slot() {
        let mut g = FrameGrid::new(60).unwrap();
        assert_eq!(g.snap(0), 0);
        // Frame 1 missing: frame 2 stays on slot 2.
        assert_eq!(g.snap(2 * P60 + 1_000_000), 33_333_333);
        // Two frames arrive together: the second takes the next slot.
        assert_eq!(g.snap(3 * P60), 50_000_000);
        assert_eq!(g.snap(3 * P60 + 500_000), 66_666_667);
        assert_eq!(g.snap(5 * P60), 83_333_333);
        assert_eq!(g.restarts(), 0);
    }

    #[test]
    fn source_faster_than_grid_restarts_instead_of_drifting() {
        let mut g = FrameGrid::new(30).unwrap();
        let mut last = Nanos::MIN;
        let mut out = Vec::new();
        // 60 frames per second into a 30 fps grid for 2 seconds.
        for i in 0..120 {
            let pts = i * P60;
            let t = g.snap(pts);
            assert!(t > last, "strictly increasing");
            assert!(
                t - pts <= NANOS_PER_SEC / 30 * 5 / 4,
                "at most 1.25 periods ahead"
            );
            last = t;
            out.push(t);
        }
        assert!(g.restarts() > 0);
    }

    #[test]
    fn time_going_backwards_or_huge_jumps_do_not_panic() {
        let mut g = FrameGrid::new(60).unwrap();
        let mut last = Nanos::MIN;
        for pts in [
            1_000_000_000,
            500_000_000,
            Nanos::MAX,
            Nanos::MIN,
            0,
            Nanos::MAX - 1,
            1,
        ] {
            let t = g.snap(pts);
            assert!(t > last || last == Nanos::MAX, "{t} after {last}");
            last = t;
        }
        assert!(FrameGrid::new(0).is_none());
    }
}
