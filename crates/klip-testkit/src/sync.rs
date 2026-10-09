//! A/V sync analysis of a recorded clip against the test game's own logs.
//!
//! The test game logs when it presented each frame (`counter,ns`) and when each beep is
//! due to play (`index,ns`), both on its own clock. In the clip, every video frame carries
//! its counter as a barcode, so the median of `clip time - game time` over all frames maps
//! the game clock onto the clip timeline (this absorbs capture latency and the different
//! clock origins, and is precise to well under a millisecond). Each beep is then expected
//! at `game time + that offset`; the A/V error is how far the detected onset is from it.

use std::collections::HashMap;
use std::fmt;

/// Result of [`analyze`].
#[derive(Clone, Debug)]
pub struct SyncReport {
    /// Median `clip time - game time` of the video frames, in seconds.
    pub video_offset_s: f64,
    /// Video frames whose counter was found in the game log.
    pub frames_used: usize,
    /// Beeps that fall inside the clip.
    pub beeps_expected: usize,
    /// Audio error (onset minus expected) of each matched beep, in milliseconds. Positive
    /// means the audio is late.
    pub errors_ms: Vec<f64>,
    /// Detected onsets that matched no beep.
    pub stray_onsets: usize,
}

impl SyncReport {
    pub fn matched(&self) -> usize {
        self.errors_ms.len()
    }

    pub fn mean_ms(&self) -> f64 {
        if self.errors_ms.is_empty() {
            return f64::NAN;
        }
        self.errors_ms.iter().sum::<f64>() / self.errors_ms.len() as f64
    }

    pub fn max_abs_ms(&self) -> f64 {
        self.errors_ms.iter().fold(0.0, |m, e| m.max(e.abs()))
    }

    pub fn min_ms(&self) -> f64 {
        self.errors_ms.iter().copied().fold(f64::INFINITY, f64::min)
    }

    pub fn max_ms(&self) -> f64 {
        self.errors_ms
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
    }
}

impl fmt::Display for SyncReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{} beeps matched ({} stray onsets), audio error mean {:+.2} ms, range {:+.2}..{:+.2} ms (video offset {:.4} s from {} frames)",
            self.matched(),
            self.beeps_expected,
            self.stray_onsets,
            self.mean_ms(),
            self.min_ms(),
            self.max_ms(),
            self.video_offset_s,
            self.frames_used
        )
    }
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

/// Parse a `key,nanoseconds` log (the test game's frame and beep logs) into seconds.
pub fn parse_log(text: &str) -> HashMap<u32, f64> {
    text.lines()
        .filter_map(|l| {
            let (k, ns) = l.trim().split_once(',')?;
            Some((k.parse().ok()?, ns.parse::<u64>().ok()? as f64 / 1e9))
        })
        .collect()
}

/// `frames`: (barcode counter, presentation time in the clip) of every decoded frame;
/// `game_frames`: counter → game time; `beeps`: game times of beep onsets; `onsets`:
/// detected onset times in the clip. Onsets further than `window_s` from any expected
/// beep count as stray. Returns `None` if no frame could be mapped.
pub fn analyze(
    frames: &[(Option<u32>, f64)],
    game_frames: &HashMap<u32, f64>,
    beeps: &[f64],
    onsets: &[f64],
    window_s: f64,
) -> Option<SyncReport> {
    let mut diffs: Vec<f64> = frames
        .iter()
        .filter_map(|(c, t)| Some(t - game_frames.get(&(*c)?)?))
        .collect();
    let frames_used = diffs.len();
    let video_offset_s = median(&mut diffs)?;
    let first = frames.iter().map(|f| f.1).fold(f64::INFINITY, f64::min);
    let last = frames.iter().map(|f| f.1).fold(f64::NEG_INFINITY, f64::max);
    let mut used = vec![false; onsets.len()];
    let mut errors_ms = Vec::new();
    let mut beeps_expected = 0;
    for b in beeps {
        let expected = b + video_offset_s;
        // Only beeps that start inside the clip and whose detection isn't cut off.
        if expected < first + window_s || expected > last - window_s {
            continue;
        }
        beeps_expected += 1;
        let best = onsets
            .iter()
            .enumerate()
            .filter(|(i, o)| used.get(*i) == Some(&false) && (*o - expected).abs() <= window_s)
            .min_by(|a, b| (a.1 - expected).abs().total_cmp(&(b.1 - expected).abs()));
        if let Some((i, o)) = best {
            if let Some(u) = used.get_mut(i) {
                *u = true;
            }
            errors_ms.push((o - expected) * 1000.0);
        }
    }
    let stray_onsets = onsets
        .iter()
        .zip(&used)
        .filter(|(o, u)| !**u && **o > first + window_s && **o < last - window_s)
        .count();
    Some(SyncReport {
        video_offset_s,
        frames_used,
        beeps_expected,
        errors_ms,
        stray_onsets,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn recovers_a_known_audio_delay() {
        // Game frames at 120 fps; the clip captured every other one, 0.75 s after the
        // game clock, with 2 ms of jitter.
        let game: HashMap<u32, f64> = (0..2400).map(|c| (c, c as f64 / 120.0)).collect();
        let frames: Vec<(Option<u32>, f64)> = (0..1200)
            .map(|i| {
                let c = 2 * i;
                let jitter = if i % 2 == 0 { 0.002 } else { -0.002 };
                (Some(c), c as f64 / 120.0 + 0.75 + jitter)
            })
            .chain([(None, 0.5)])
            .collect();
        let beeps: Vec<f64> = (1..20).map(f64::from).collect();
        // Audio 7 ms late, one beep missing, one stray click.
        let mut onsets: Vec<f64> = beeps
            .iter()
            .filter(|b| **b != 5.0)
            .map(|b| b + 0.75 + 0.007)
            .collect();
        onsets.push(9.5);
        let r = analyze(&frames, &game, &beeps, &onsets, 0.25).unwrap();
        assert_eq!(r.frames_used, 1200);
        assert!((r.video_offset_s - 0.75).abs() < 0.0025);
        assert_eq!(r.beeps_expected, 19);
        assert_eq!(r.matched(), 18);
        assert_eq!(r.stray_onsets, 1);
        assert!((r.mean_ms() - 7.0).abs() < 2.5, "{r}");
    }

    #[test]
    fn parse_logs() {
        let m = parse_log("0,0\n1,8333333\nbad\n2,x\n\n3,25000000\n");
        assert_eq!(m.len(), 3);
        assert!((m[&1] - 0.008_333_333).abs() < 1e-12);
        assert!(analyze(&[], &m, &[], &[], 0.1).is_none());
    }
}
