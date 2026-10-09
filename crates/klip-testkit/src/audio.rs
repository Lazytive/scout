//! Audio test signals and an onset detector.
//!
//! The test game and the synthetic audio test play short tone bursts ("beeps") that start
//! at full amplitude, so the start of each burst can be found to within a fraction of a
//! millisecond in the decoded clip, even after AAC (whose pre-echo stays well below the
//! detection threshold at the bitrates Klip uses).

use std::f64::consts::TAU;

/// Frequency of the test beep.
pub const BEEP_HZ: f64 = 1000.0;
/// Length of the test beep.
pub const BEEP_MS: u32 = 60;
/// Peak amplitude of the test beep, as a fraction of full scale.
pub const BEEP_AMPLITUDE: f64 = 0.25;
/// Onset threshold used on decoded audio, as a fraction of full scale.
pub const ONSET_THRESHOLD: f64 = 0.1;

/// Value of the beep `n` samples after its start, or `None` past its end. Starts at the
/// sine's peak (a cosine), so the first sample is already above the onset threshold.
pub fn beep_sample(n: u64, rate: u32) -> Option<f64> {
    let len = u64::from(rate) * u64::from(BEEP_MS) / 1000;
    if n >= len || rate == 0 {
        return None;
    }
    let t = n as f64 / f64::from(rate);
    Some(BEEP_AMPLITUDE * (TAU * BEEP_HZ * t).cos())
}

/// Interleaved S16 PCM: silence with a beep starting at each of `onsets_s`.
pub fn render_beeps_s16(rate: u32, channels: u16, total_s: f64, onsets_s: &[f64]) -> Vec<i16> {
    let frames = (total_s * f64::from(rate)).round().max(0.0) as usize;
    let mut out = vec![0i16; frames * usize::from(channels)];
    for &start in onsets_s {
        let first = (start * f64::from(rate)).round().max(0.0) as usize;
        let mut n = 0u64;
        while let Some(v) = beep_sample(n, rate) {
            let frame = first + n as usize;
            if frame >= frames {
                break;
            }
            let s = (v * f64::from(i16::MAX)).round() as i16;
            let at = frame * usize::from(channels);
            if let Some(slots) = out.get_mut(at..at + usize::from(channels)) {
                slots.fill(s);
            }
            n += 1;
        }
    }
    out
}

/// Times (seconds from the first sample) where `|x|` first exceeds `threshold` (fraction
/// of full scale) after at least `quiet_s` below it.
pub fn onsets(samples: &[i16], rate: u32, threshold: f64, quiet_s: f64) -> Vec<f64> {
    if rate == 0 {
        return Vec::new();
    }
    let limit = (threshold * f64::from(i16::MAX)) as i32;
    let quiet = (quiet_s * f64::from(rate)) as usize;
    let mut out = Vec::new();
    let mut last_loud: Option<usize> = None;
    for (i, &s) in samples.iter().enumerate() {
        if i32::from(s).abs() <= limit {
            continue;
        }
        let is_new = match last_loud {
            None => true,
            Some(l) => i - l > quiet,
        };
        if is_new {
            out.push(i as f64 / f64::from(rate));
        }
        last_loud = Some(i);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn rendered_beeps_are_found_to_the_sample() {
        let rate = 48_000;
        let at = [0.5, 1.25, 2.0];
        let pcm = render_beeps_s16(rate, 2, 3.0, &at);
        let mono: Vec<i16> = pcm.chunks(2).map(|c| c[0]).collect();
        let found = onsets(&mono, rate, ONSET_THRESHOLD, 0.2);
        assert_eq!(found.len(), 3, "{found:?}");
        for (f, a) in found.iter().zip(at) {
            assert!((f - a).abs() < 1.0 / f64::from(rate), "{f} vs {a}");
        }
    }

    #[test]
    fn quiet_input_has_no_onsets() {
        assert!(onsets(&[0i16; 48_000], 48_000, ONSET_THRESHOLD, 0.2).is_empty());
        assert!(onsets(&[], 0, ONSET_THRESHOLD, 0.2).is_empty());
        assert!(beep_sample(0, 0).is_none());
    }
}
