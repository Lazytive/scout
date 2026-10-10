//! The sound of an export. Each played piece of a layer arrives as its decoded tracks;
//! [`piece_mix`] weighs them (Normal mode's Game/Mic sliders for the main clip, 1 for
//! anything else), applies the layer's volume keyframes per millisecond, and splits off
//! the "voice" that ducks the music (microphone tracks and sound/voice-over layers;
//! game audio does not duck). The [`Mixer`] sums pieces into two buses (music, and
//! everything else) on the output timeline and hands out finished audio as soon as no
//! running piece can still add to it, so memory stays bounded by the longest piece.
//!
//! Ducking: the voice's level per 10 ms block (RMS, dBFS) gates a target gain (`duck_db`
//! while it is above [`VOICE_GATE_DB`], else 1); the gain follows the target with
//! one-pole attack/release per sample and scales the music bus.

use klip_edit::Volumes;
use klip_edit::advanced::Ducking;

use crate::source::SAMPLES_PER_MS;

/// Ducking looks at the voice in blocks of this length.
pub const BLOCK_MS: u64 = 10;
/// The voice counts as speaking above this level.
pub const VOICE_GATE_DB: f32 = -45.0;
const RATE_PER_MS: f32 = 48.0;

/// Which slider a recorded track follows (`None`: not used in the mix).
pub fn volume_for(name: &str, has_separate: bool, v: &Volumes) -> Option<f32> {
    match name {
        "Game" | "System" => Some(v.game),
        "Microphone" | "Voice" => Some(v.mic),
        _ => (!has_separate).then_some(v.game),
    }
}

/// Whether a recorded track is someone speaking.
pub fn is_voice(name: &str) -> bool {
    matches!(name, "Microphone" | "Voice")
}

/// The samples of one piece trimmed or padded to its output length.
pub fn fit_samples(mut samples: Vec<i16>, out_ms: u64) -> Vec<i16> {
    let want = usize::try_from(out_ms)
        .unwrap_or(usize::MAX)
        .saturating_mul(SAMPLES_PER_MS);
    samples.resize(want, 0);
    samples
}

fn gain_of(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 4.0)
    } else {
        1.0
    }
}

/// One piece's tracks as (mix, voice), both `len_ms` long, interleaved stereo:
/// `weights[i]` scales track `i` (`None` leaves it out), `voice[i]` marks the voice
/// tracks (or `all_voice` for a sound/voice-over layer), and `gain_at(ms)` is the
/// layer's volume at the piece's `ms`-th millisecond.
pub fn piece_mix(
    tracks: Vec<Vec<i16>>,
    weights: &[Option<f32>],
    voice: &[bool],
    all_voice: bool,
    len_ms: u64,
    gain_at: impl Fn(u64) -> f32,
) -> (Vec<f32>, Vec<f32>) {
    let n = usize::try_from(len_ms)
        .unwrap_or(0)
        .saturating_mul(SAMPLES_PER_MS);
    let mut mix = vec![0.0f32; n];
    let mut speech = vec![0.0f32; n];
    let mut any_voice = false;
    for (i, track) in tracks.into_iter().enumerate() {
        let Some(w) = weights.get(i).copied().flatten().map(gain_of) else {
            continue;
        };
        if w == 0.0 {
            continue;
        }
        let track = fit_samples(track, len_ms);
        let is_voice = all_voice || voice.get(i).copied().unwrap_or(false);
        any_voice |= is_voice;
        for ((m, s), &v) in mix.iter_mut().zip(speech.iter_mut()).zip(&track) {
            let x = f32::from(v) * w;
            *m += x;
            if is_voice {
                *s += x;
            }
        }
    }
    for (ms, (m, s)) in mix
        .chunks_mut(SAMPLES_PER_MS)
        .zip(speech.chunks_mut(SAMPLES_PER_MS))
        .enumerate()
    {
        let g = gain_of(gain_at(ms as u64));
        if g == 1.0 {
            continue;
        }
        m.iter_mut().for_each(|x| *x *= g);
        s.iter_mut().for_each(|x| *x *= g);
    }
    if !any_voice {
        speech.clear();
    }
    (mix, speech)
}

/// Where a piece's sound goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Main,
    Music,
}

#[derive(Debug)]
struct Duck {
    floor: f32,
    attack: f32,
    release: f32,
    gain: f32,
}

/// One-pole coefficient per sample frame for a time constant of `ms`.
fn coef(ms: u32) -> f32 {
    if ms == 0 {
        return 1.0;
    }
    1.0 - (-1.0 / (ms as f32 * RATE_PER_MS)).exp()
}

/// The two buses and the voice detector, from `base_ms` on.
#[derive(Debug)]
pub struct Mixer {
    base_ms: u64,
    main: Vec<f32>,
    music: Vec<f32>,
    voice: Vec<f32>,
    duck: Option<Duck>,
}

fn add_into(buf: &mut Vec<f32>, offset: usize, samples: &[f32]) {
    let end = offset.saturating_add(samples.len());
    if buf.len() < end {
        buf.resize(end, 0.0);
    }
    if let Some(dst) = buf.get_mut(offset..end) {
        for (d, s) in dst.iter_mut().zip(samples) {
            *d += *s;
        }
    }
}

fn take_front(buf: &mut Vec<f32>, n: usize) -> Vec<f32> {
    let have = n.min(buf.len());
    let mut out: Vec<f32> = buf.drain(..have).collect();
    out.resize(n, 0.0);
    out
}

impl Mixer {
    pub fn new(ducking: &Ducking) -> Mixer {
        let duck = ducking.enabled.then(|| Duck {
            floor: if ducking.duck_db.is_finite() {
                10f32.powf(ducking.duck_db.clamp(-60.0, 0.0) / 20.0)
            } else {
                1.0
            },
            attack: coef(ducking.attack_ms.min(10_000)),
            release: coef(ducking.release_ms.min(10_000)),
            gain: 1.0,
        });
        Mixer {
            base_ms: 0,
            main: Vec::new(),
            music: Vec::new(),
            voice: Vec::new(),
            duck,
        }
    }

    /// Output time up to which audio has been handed out.
    pub fn flushed_ms(&self) -> u64 {
        self.base_ms
    }

    /// Add a piece starting at output `at_ms`: its mix to `bus`, its voice (may be
    /// empty) to the ducking detector. What lies before [`Self::flushed_ms`] is lost
    /// (nothing should arrive that late).
    pub fn add(&mut self, bus: Bus, at_ms: u64, mix: &[f32], voice: &[f32]) {
        let (skip, offset) = if at_ms >= self.base_ms {
            (0, at_ms.saturating_sub(self.base_ms))
        } else {
            (self.base_ms.saturating_sub(at_ms), 0)
        };
        let skip = usize::try_from(skip)
            .unwrap_or(usize::MAX)
            .saturating_mul(SAMPLES_PER_MS);
        let offset = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .saturating_mul(SAMPLES_PER_MS);
        let target = match bus {
            Bus::Main => &mut self.main,
            Bus::Music => &mut self.music,
        };
        add_into(target, offset, mix.get(skip..).unwrap_or_default());
        add_into(
            &mut self.voice,
            offset,
            voice.get(skip..).unwrap_or_default(),
        );
    }

    /// The finished audio from [`Self::flushed_ms`] to `until_ms` (rounded down to a
    /// whole ducking block unless `last`, which pads with silence to `until_ms`), as
    /// 16-bit samples for the encoder.
    pub fn flush(&mut self, until_ms: u64, last: bool) -> Vec<i16> {
        let mut end = until_ms.max(self.base_ms);
        if !last {
            end = end.saturating_sub(end % BLOCK_MS).max(self.base_ms);
        }
        let len_ms = end.saturating_sub(self.base_ms);
        if len_ms == 0 {
            return Vec::new();
        }
        let n = usize::try_from(len_ms)
            .unwrap_or(0)
            .saturating_mul(SAMPLES_PER_MS);
        let main = take_front(&mut self.main, n);
        let mut music = take_front(&mut self.music, n);
        let voice = take_front(&mut self.voice, n);
        if let Some(d) = self.duck.as_mut() {
            let block = usize::try_from(BLOCK_MS)
                .unwrap_or(10)
                .saturating_mul(SAMPLES_PER_MS);
            for (m, v) in music.chunks_mut(block).zip(voice.chunks(block)) {
                let mean_sq = v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>()
                    / v.len().max(1) as f64;
                let db = 10.0 * (mean_sq / (32768.0f64 * 32768.0)).max(1e-12).log10();
                let target = if db as f32 > VOICE_GATE_DB {
                    d.floor
                } else {
                    1.0
                };
                for frame in m.chunks_mut(2) {
                    let c = if target < d.gain { d.attack } else { d.release };
                    d.gain += (target - d.gain) * c;
                    frame.iter_mut().for_each(|x| *x *= d.gain);
                }
            }
        }
        self.base_ms = end;
        main.iter()
            .zip(&music)
            .map(|(a, b)| (a + b).clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(ms: u64, amp: f32) -> Vec<f32> {
        (0..(ms as usize).saturating_mul(SAMPLES_PER_MS))
            .map(|i| amp * ((i / 2) as f32 * 0.1).sin())
            .collect()
    }

    fn rms(s: &[i16]) -> f64 {
        (s.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / s.len().max(1) as f64).sqrt()
    }

    #[test]
    fn volumes_pick_the_tracks() {
        let v = Volumes {
            game: 0.5,
            mic: 2.0,
        };
        assert_eq!(volume_for("Game", true, &v), Some(0.5));
        assert_eq!(volume_for("Microphone", true, &v), Some(2.0));
        assert_eq!(volume_for("Mix", true, &v), None);
        assert_eq!(volume_for("Mix", false, &v), Some(0.5));
        assert_eq!(volume_for("", false, &v), Some(0.5));
        assert!(is_voice("Microphone") && !is_voice("Game"));
    }

    #[test]
    fn samples_fit_their_piece() {
        assert_eq!(fit_samples(vec![1; 10], 1).len(), SAMPLES_PER_MS);
        assert_eq!(fit_samples(vec![], 3).len(), 3 * SAMPLES_PER_MS);
    }

    #[test]
    fn piece_mix_weighs_and_keys_volume() {
        let a = vec![100i16; 4 * SAMPLES_PER_MS];
        let b = vec![10i16; 2 * SAMPLES_PER_MS];
        let (mix, voice) = piece_mix(
            vec![a, b],
            &[Some(1.0), Some(2.0)],
            &[false, true],
            false,
            4,
            |ms| if ms >= 2 { 0.5 } else { 1.0 },
        );
        assert_eq!(mix.len(), 4 * SAMPLES_PER_MS);
        assert_eq!(mix[0], 120.0);
        assert_eq!(mix[3 * SAMPLES_PER_MS], 50.0, "b ended, half volume");
        assert_eq!(voice[0], 20.0);
        assert_eq!(voice[3 * SAMPLES_PER_MS], 0.0);
        // No voice track: no voice at all; a left-out track is silent.
        let (mix, voice) = piece_mix(vec![vec![7i16; 96]], &[None], &[true], false, 1, |_| 1.0);
        assert!(mix.iter().all(|v| *v == 0.0) && voice.is_empty());
        // Hostile gains are tamed.
        let (mix, _) = piece_mix(
            vec![vec![1i16; 96]],
            &[Some(f32::NAN)],
            &[],
            true,
            1,
            |_| 99.0,
        );
        assert_eq!(mix[0], 4.0);
    }

    #[test]
    fn mixer_flushes_whole_blocks_and_sums() {
        let mut m = Mixer::new(&Ducking {
            enabled: false,
            ..Ducking::default()
        });
        m.add(Bus::Main, 0, &vec![1000.0; 25 * SAMPLES_PER_MS], &[]);
        m.add(Bus::Music, 5, &vec![500.0; 10 * SAMPLES_PER_MS], &[]);
        let a = m.flush(23, false);
        assert_eq!(a.len(), 20 * SAMPLES_PER_MS, "two whole blocks");
        assert_eq!(a[0], 1000);
        assert_eq!(a[6 * SAMPLES_PER_MS], 1500);
        assert_eq!(m.flushed_ms(), 20);
        let b = m.flush(30, true);
        assert_eq!(b.len(), 10 * SAMPLES_PER_MS);
        assert_eq!(b[0], 1000);
        assert_eq!(b[9 * SAMPLES_PER_MS], 0, "padded with silence");
        // Late audio is cut to what is still open.
        m.add(Bus::Main, 25, &vec![9.0; 10 * SAMPLES_PER_MS], &[]);
        let c = m.flush(35, true);
        assert_eq!(c.len(), 5 * SAMPLES_PER_MS);
        assert_eq!(c[0], 9);
        assert!(m.flush(10, true).is_empty());
        let mut loud = Mixer::new(&Ducking::default());
        loud.add(Bus::Main, 0, &[40_000.0, -40_000.0], &[]);
        assert_eq!(&loud.flush(1, true)[..2], &[i16::MAX, i16::MIN]);
    }

    #[test]
    fn music_ducks_under_the_voice() {
        let ducking = Ducking::default();
        let mut m = Mixer::new(&ducking);
        let music = tone(2_000, 8_000.0);
        m.add(Bus::Music, 0, &music, &[]);
        // Speech from 500 to 1500 ms.
        let speech = tone(1_000, 6_000.0);
        m.add(Bus::Main, 500, &vec![0.0; speech.len()], &speech);
        let out = m.flush(2_000, true);
        let at = |ms: usize| &out[ms * SAMPLES_PER_MS..(ms + 100) * SAMPLES_PER_MS];
        let before = rms(at(300));
        let during = rms(at(1_300));
        let after = rms(at(1_890));
        let db = 20.0 * (during / before).log10();
        assert!((db + 12.0).abs() < 0.5, "ducked by {db:.2} dB");
        assert!(after > during * 2.0, "released: {after} vs {during}");
        // Silence does not duck.
        let mut quiet = Mixer::new(&ducking);
        quiet.add(Bus::Music, 0, &music, &[]);
        quiet.add(Bus::Main, 0, &[], &vec![1.0; music.len()]);
        let q = quiet.flush(2_000, true);
        assert!((rms(&q[..SAMPLES_PER_MS * 1_000]) / before - 1.0).abs() < 0.01);
    }
}
