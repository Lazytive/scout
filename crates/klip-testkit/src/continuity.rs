//! Frame-continuity analysis of decoded barcode counters.

use std::collections::BTreeMap;
use std::fmt;

/// Result of looking at the counter sequence of a clip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContinuityReport {
    pub frames: usize,
    pub unreadable: usize,
    /// Consecutive frames with the same counter (a repeated frame).
    pub duplicates: usize,
    /// Counter went backwards (reordering bug).
    pub backwards: usize,
    /// Histogram of counter steps between consecutive readable frames.
    pub steps: BTreeMap<u32, usize>,
    pub first: Option<u32>,
    pub last: Option<u32>,
}

impl ContinuityReport {
    /// Every frame readable and every step exactly 1.
    pub fn is_exact(&self) -> bool {
        self.unreadable == 0
            && self.duplicates == 0
            && self.backwards == 0
            && self.steps.keys().all(|s| *s == 1)
    }

    /// Largest step between consecutive readable frames.
    pub fn max_step(&self) -> u32 {
        self.steps.keys().next_back().copied().unwrap_or(0)
    }

    /// Steps strictly greater than `limit` (likely dropped frames).
    pub fn steps_over(&self, limit: u32) -> usize {
        self.steps
            .range(limit.saturating_add(1)..)
            .map(|(_, n)| *n)
            .sum()
    }
}

impl fmt::Display for ContinuityReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "frames={} unreadable={} duplicates={} backwards={} first={:?} last={:?} steps={:?}",
            self.frames,
            self.unreadable,
            self.duplicates,
            self.backwards,
            self.first,
            self.last,
            self.steps
        )
    }
}

/// Analyse a counter sequence (one entry per decoded frame).
pub fn analyze(counters: &[Option<u32>]) -> ContinuityReport {
    let mut r = ContinuityReport {
        frames: counters.len(),
        ..Default::default()
    };
    let mut prev: Option<u32> = None;
    for c in counters {
        let Some(c) = *c else {
            r.unreadable = r.unreadable.saturating_add(1);
            continue;
        };
        if r.first.is_none() {
            r.first = Some(c);
        }
        r.last = Some(c);
        if let Some(p) = prev {
            if c == p {
                r.duplicates = r.duplicates.saturating_add(1);
            } else if c < p {
                r.backwards = r.backwards.saturating_add(1);
            } else {
                let e = r.steps.entry(c.saturating_sub(p)).or_insert(0);
                *e = e.saturating_add(1);
            }
        }
        prev = Some(c);
    }
    r
}

/// How many repeated frames a constant-rate capture at `period_s` may legitimately
/// contain between game frames `first..=last`: a game frame that stays on screen for
/// `g` seconds spans up to `floor(g / period_s)` capture ticks without a new frame.
/// `game_times`: counter → time the game finished the frame (its frame log, seconds).
/// Counters missing from the log are skipped.
pub fn allowed_repeats(
    game_times: &std::collections::HashMap<u32, f64>,
    first: u32,
    last: u32,
    period_s: f64,
) -> usize {
    if period_s <= 0.0 || last <= first {
        return 0;
    }
    let times: Vec<f64> = (first..=last)
        .filter_map(|c| game_times.get(&c).copied())
        .collect();
    times
        .windows(2)
        .map(|w| {
            let gap = match w {
                [a, b] => b - a,
                _ => 0.0,
            };
            if gap > period_s {
                (gap / period_s).floor() as usize
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_follow_game_stalls() {
        let p = 1.0 / 60.0;
        // A steady 120 fps game never forces a repeat.
        let steady: std::collections::HashMap<u32, f64> =
            (0..240).map(|c| (c, f64::from(c) / 120.0)).collect();
        assert_eq!(allowed_repeats(&steady, 0, 239, p), 0);
        // One 50 ms stall spans up to 3 capture ticks without a new frame.
        let mut stall = steady.clone();
        for c in 100..240 {
            *stall.get_mut(&c).unwrap() += 0.05;
        }
        assert_eq!(allowed_repeats(&stall, 0, 239, p), 3);
        assert_eq!(allowed_repeats(&stall, 0, 99, p), 0);
        assert_eq!(allowed_repeats(&stall, 5, 5, p), 0);
        assert_eq!(allowed_repeats(&stall, 0, 239, 0.0), 0);
    }

    #[test]
    fn exact_sequences() {
        let seq: Vec<_> = (10..20).map(Some).collect();
        let r = analyze(&seq);
        assert!(r.is_exact(), "{r}");
        assert_eq!((r.first, r.last, r.max_step()), (Some(10), Some(19), 1));
    }

    #[test]
    fn detects_drops_duplicates_and_garbage() {
        let r = analyze(&[Some(1), Some(2), Some(2), Some(5), None, Some(4), Some(6)]);
        assert!(!r.is_exact());
        assert_eq!(r.duplicates, 1);
        assert_eq!(r.unreadable, 1);
        assert_eq!(r.backwards, 1);
        assert_eq!(r.max_step(), 3);
        assert_eq!(r.steps_over(2), 1);
    }
}
