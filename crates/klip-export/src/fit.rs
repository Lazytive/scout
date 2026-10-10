//! Discord size-fit (ADR 0017): pick a bitrate so the file stays under a byte limit.
//! Pass 1 aims at 94 % of the limit minus the audio; if the file still comes out too
//! big, pass 2 scales the bitrate by the overshoot (and 3 %) ; a target under
//! 1.5 Mbit/s steps the picture down (720p30, then 480p30) first. Pure math, no
//! GStreamer.

/// Lowest video bitrate a pass asks for, kbit/s.
pub const MIN_KBPS: u32 = 150;
/// Under this target the picture steps down to 720p at 30 fps.
pub const STEP_720_KBPS: u32 = 1_500;
/// Under this target the picture steps down to 480p at 30 fps.
pub const STEP_480_KBPS: u32 = 700;
/// Most encode passes before giving up.
pub const MAX_PASSES: u32 = 3;

/// What one pass encodes with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FitPass {
    pub bitrate_kbps: u32,
    /// Frame height cap (the width follows the aspect ratio).
    pub max_height: u32,
    pub fps_cap: u32,
}

fn with_steps(bitrate_kbps: u32) -> FitPass {
    let bitrate_kbps = bitrate_kbps.max(MIN_KBPS);
    let (max_height, fps_cap) = if bitrate_kbps < STEP_480_KBPS {
        (480, 30)
    } else if bitrate_kbps < STEP_720_KBPS {
        (720, 30)
    } else {
        (1080, 60)
    };
    FitPass {
        bitrate_kbps,
        max_height,
        fps_cap,
    }
}

/// Pass 1: `(0.94 x limit - audio bytes) / duration`.
pub fn first_pass(limit_bytes: u64, duration_ms: u64, audio_kbps: u32) -> FitPass {
    let duration_ms = duration_ms.max(1);
    let budget_bits = limit_bytes
        .saturating_mul(8)
        .saturating_mul(94)
        .checked_div(100)
        .unwrap_or(0);
    let audio_bits = u64::from(audio_kbps)
        .saturating_mul(duration_ms)
        .checked_div(1)
        .unwrap_or(0); // kbit/s x ms = bits
    let video_bits = budget_bits.saturating_sub(audio_bits);
    let kbps = video_bits.checked_div(duration_ms).unwrap_or(0);
    with_steps(u32::try_from(kbps).unwrap_or(u32::MAX))
}

/// The next pass after one that produced `actual_bytes` over `limit_bytes`:
/// `target x limit / actual x 0.97`.
pub fn next_pass(prev: &FitPass, limit_bytes: u64, actual_bytes: u64) -> FitPass {
    let actual = actual_bytes.max(1);
    let scaled = u64::from(prev.bitrate_kbps)
        .saturating_mul(limit_bytes)
        .saturating_mul(97)
        .checked_div(actual.saturating_mul(100))
        .unwrap_or(0);
    // Always go down: a pass that didn't fit must not be repeated.
    let kbps = u32::try_from(scaled)
        .unwrap_or(u32::MAX)
        .min(prev.bitrate_kbps.saturating_sub(prev.bitrate_kbps / 20));
    with_steps(kbps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_pass_budget() {
        // 10 MB, 20 s, 128 kbit/s audio: (75.2 Mbit - 2.56 Mbit) / 20 s = 3632 kbit/s.
        let p = first_pass(10_000_000, 20_000, 128);
        assert_eq!(p.bitrate_kbps, 3_632);
        assert_eq!((p.max_height, p.fps_cap), (1080, 60));
        // 60 s: (75.2 Mbit - 7.68 Mbit) / 60 s = 1125 kbit/s → 720p30.
        let p = first_pass(10_000_000, 60_000, 128);
        assert_eq!(p.bitrate_kbps, 1_125);
        assert_eq!((p.max_height, p.fps_cap), (720, 30));
        // 3 minutes: 289 kbit/s → 480p30.
        let p = first_pass(10_000_000, 180_000, 128);
        assert_eq!((p.max_height, p.fps_cap), (480, 30));
        // Nonsense never panics and never goes under the floor.
        assert_eq!(first_pass(0, 0, u32::MAX).bitrate_kbps, MIN_KBPS);
        assert!(first_pass(u64::MAX, u64::MAX, 0).bitrate_kbps >= MIN_KBPS);
    }

    #[test]
    fn next_pass_scales_by_the_overshoot() {
        let p1 = FitPass {
            bitrate_kbps: 3_632,
            max_height: 1080,
            fps_cap: 60,
        };
        // 12 MB instead of 10: 3632 * 10/12 * 0.97 = 2935.
        let p2 = next_pass(&p1, 10_000_000, 12_000_000);
        assert_eq!(p2.bitrate_kbps, 2_935);
        // Barely over: still at least 5 % lower.
        let p3 = next_pass(&p1, 10_000_000, 10_000_001);
        assert!(p3.bitrate_kbps <= 3_632 - 181);
        assert!(next_pass(&p1, 0, 0).bitrate_kbps >= MIN_KBPS);
        assert!(next_pass(&p1, u64::MAX, 1).bitrate_kbps < p1.bitrate_kbps);
    }
}
