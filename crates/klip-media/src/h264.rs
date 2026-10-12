//! H.264 levels (ITU-T H.264 Table A-1): what a stream signals and what its size and
//! frame rate need. A stream that signals too low a level plays in most players, but
//! some (and `h264parse`) complain; the recorder logs it so testers' logs show it.

/// `level_idc`, max macroblocks per second, max frame size in macroblocks.
const LEVELS: [(u8, u64, u64); 19] = [
    (10, 1_485, 99),
    (11, 3_000, 396),
    (12, 6_000, 396),
    (13, 11_880, 396),
    (20, 11_880, 396),
    (21, 19_800, 792),
    (22, 20_250, 1_620),
    (30, 40_500, 1_620),
    (31, 108_000, 3_600),
    (32, 216_000, 5_120),
    (40, 245_760, 8_192),
    (41, 245_760, 8_192),
    (42, 522_240, 8_704),
    (50, 589_824, 22_080),
    (51, 983_040, 36_864),
    (52, 2_073_600, 36_864),
    (60, 4_177_920, 139_264),
    (61, 8_355_840, 139_264),
    (62, 16_711_680, 139_264),
];

/// The level an `avcC` record (MP4 `codec_data`) signals, as `level_idc` (50 = 5.0).
pub fn signalled_level(avcc: &[u8]) -> Option<u8> {
    match avcc {
        [1, _, _, level, ..] => Some(*level),
        _ => None,
    }
}

/// The lowest level whose limits fit `width`x`height` at `fps_num/fps_den`, as
/// `level_idc`; `None` when even 6.2 is too low or the input makes no sense.
pub fn needed_level(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Option<u8> {
    if width == 0 || height == 0 || fps_num == 0 || fps_den == 0 {
        return None;
    }
    let mbs = u64::from(width.div_ceil(16)).checked_mul(u64::from(height.div_ceil(16)))?;
    // Macroblocks per second, rounded up: mbs * fps_num / fps_den.
    let mbps = mbs
        .checked_mul(u64::from(fps_num))?
        .div_ceil(u64::from(fps_den));
    LEVELS
        .iter()
        .find(|(_, max_mbps, max_fs)| mbs <= *max_fs && mbps <= *max_mbps)
        .map(|(l, _, _)| *l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_for_common_recordings() {
        assert_eq!(needed_level(1920, 1080, 60, 1), Some(42));
        assert_eq!(needed_level(1920, 1080, 30, 1), Some(40));
        // The AMD tester's 2560x1440 at 144 fps needs 5.2 (5.0 allows ~41 fps there:
        // h264parse's "VUI framerate 144 exceeds allowed maximum 41").
        assert_eq!(needed_level(2560, 1440, 144, 1), Some(52));
        assert_eq!(needed_level(2560, 1440, 40, 1), Some(50));
        assert_eq!(needed_level(3840, 2160, 60, 1), Some(52));
        assert_eq!(needed_level(3840, 2160, 120, 1), Some(60));
        assert_eq!(needed_level(1920, 1080, 60_000, 1_001), Some(42));
        assert_eq!(needed_level(16_384, 16_384, 240, 1), None);
        assert_eq!(needed_level(0, 1080, 60, 1), None);
        assert_eq!(needed_level(1920, 1080, 60, 0), None);
        assert_eq!(needed_level(u32::MAX, u32::MAX, u32::MAX, 1), None);
    }

    #[test]
    fn level_from_avcc() {
        assert_eq!(signalled_level(&[1, 100, 0, 50, 0xff]), Some(50));
        assert_eq!(signalled_level(&[1, 100, 0]), None);
        assert_eq!(signalled_level(&[0, 100, 0, 50]), None);
        assert_eq!(signalled_level(&[]), None);
    }
}
