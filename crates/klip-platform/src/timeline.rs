//! The audio capture timeline shared by the platform backends: every packet carries the
//! performance-clock time of its first frame ([`crate::qpc_now_ns`]); while a source
//! delivers nothing, silence is generated so the stream stays continuous; real packets
//! that overlap generated silence are trimmed.

use crate::{AudioCaptureSink, AudioPacket, qpc_now_ns};

/// Generate silence once nothing has arrived for this long…
pub(crate) const SILENCE_AFTER_NS: i64 = 60_000_000;
/// …up to this long before now, leaving room for late packets.
pub(crate) const SILENCE_MARGIN_NS: i64 = 40_000_000;
/// Silence comes in packets of at most this length.
pub(crate) const SILENCE_PACKET_NS: i64 = 10_000_000;
/// How often a failed device is retried.
pub(crate) const RETRY_NS: i64 = 2_000_000_000;
/// Overlap with already delivered audio that gets trimmed (and gaps that get filled).
pub(crate) const OVERLAP_NS: i64 = 1_000_000;
/// Longest gap before a packet that is filled with silence; a larger jump is passed on as
/// a discontinuity instead.
pub(crate) const MAX_FILL_NS: i64 = 10_000_000_000;

/// Interleaved 32-bit float at `rate` Hz x `channels`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Format {
    pub(crate) rate: u32,
    pub(crate) channels: u16,
}

impl Format {
    pub(crate) fn frames_ns(self, frames: u32) -> i64 {
        i64::from(frames)
            .saturating_mul(1_000_000_000)
            .checked_div(i64::from(self.rate))
            .unwrap_or(0)
    }

    pub(crate) fn ns_frames(self, ns: i64) -> u32 {
        let f = ns
            .max(0)
            .saturating_mul(i64::from(self.rate))
            .checked_div(1_000_000_000)
            .unwrap_or(0);
        u32::try_from(f).unwrap_or(u32::MAX)
    }
}

/// Hand a packet to the sink, trimming any part that overlaps audio already delivered
/// (silence generated while the stream was quiet).
pub(crate) fn deliver(
    fmt: Format,
    next: &mut Option<i64>,
    start: i64,
    frames: u32,
    samples: Option<&[f32]>,
    discontinuity: bool,
    sink: &mut dyn AudioCaptureSink,
) {
    // Audio resuming after a quiet spell can start after the silence generated so far
    // (that stops `SILENCE_MARGIN_NS` before now): fill the gap so the stream stays
    // continuous. Seen on PipeWire when the graph changes quantum or driver.
    let mut discontinuity = discontinuity;
    if let Some(n) = *next {
        let gap = start.saturating_sub(n);
        if gap > MAX_FILL_NS {
            discontinuity = true;
        } else if gap > OVERLAP_NS {
            let mut t = n;
            while start.saturating_sub(t) > OVERLAP_NS {
                let frames = fmt.ns_frames(start.saturating_sub(t).min(SILENCE_PACKET_NS));
                if frames == 0 {
                    break;
                }
                sink.packet(AudioPacket {
                    qpc_ns: t,
                    frames,
                    data: None,
                    discontinuity: false,
                });
                t = t.saturating_add(fmt.frames_ns(frames));
            }
            *next = Some(t);
        }
    }
    let skip = match *next {
        Some(n) if start < n.saturating_sub(OVERLAP_NS) => {
            fmt.ns_frames(n.saturating_sub(start)).min(frames)
        }
        _ => 0,
    };
    let keep = frames.saturating_sub(skip);
    if keep == 0 {
        return;
    }
    let start = start.saturating_add(fmt.frames_ns(skip));
    let data = samples.and_then(|s| {
        let from = usize::try_from(skip)
            .ok()?
            .checked_mul(usize::from(fmt.channels))?;
        s.get(from..)
    });
    sink.packet(AudioPacket {
        qpc_ns: start,
        frames: keep,
        data,
        discontinuity,
    });
    *next = Some(start.saturating_add(fmt.frames_ns(keep)));
}

/// Keep the stream continuous while nothing arrives.
pub(crate) fn fill_silence(fmt: Format, next: &mut Option<i64>, sink: &mut dyn AudioCaptureSink) {
    let Some(mut t) = *next else { return };
    let now = qpc_now_ns();
    if now.saturating_sub(t) < SILENCE_AFTER_NS {
        return;
    }
    let until = now.saturating_sub(SILENCE_MARGIN_NS);
    while t < until {
        let frames = fmt.ns_frames(until.saturating_sub(t).min(SILENCE_PACKET_NS));
        if frames == 0 {
            break;
        }
        sink.packet(AudioPacket {
            qpc_ns: t,
            frames,
            data: None,
            discontinuity: false,
        });
        t = t.saturating_add(fmt.frames_ns(frames));
    }
    *next = Some(t);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<(i64, u32, bool, bool)>);
    impl AudioCaptureSink for Log {
        fn packet(&mut self, p: AudioPacket<'_>) {
            self.0
                .push((p.qpc_ns, p.frames, p.data.is_some(), p.discontinuity));
        }
        fn notice(&mut self, _: String) {}
    }

    const FMT: Format = Format {
        rate: 48_000,
        channels: 2,
    };

    fn end(p: &(i64, u32, bool, bool)) -> i64 {
        p.0 + FMT.frames_ns(p.1)
    }

    #[test]
    fn gaps_after_silence_are_filled() {
        // Regression: PipeWire resumed 41.6 ms after the generated silence ended.
        let mut log = Log::default();
        let mut next = Some(1_000_000_000);
        let samples = vec![0.5f32; 2048 * 2];
        deliver(
            FMT,
            &mut next,
            1_041_600_000,
            2048,
            Some(&samples),
            false,
            &mut log,
        );
        let real = log.0.last().unwrap();
        assert!(real.2, "the real packet comes last");
        assert_eq!(log.0.first().unwrap().0, 1_000_000_000);
        for w in log.0.windows(2) {
            assert!((w[1].0 - end(&w[0])).abs() < OVERLAP_NS, "{:?}", log.0);
        }
        assert!(log.0[..log.0.len() - 1].iter().all(|p| !p.2 && !p.3));
        assert_eq!(next, Some(end(real)));
    }

    #[test]
    fn overlaps_are_trimmed_and_big_jumps_flagged() {
        let mut log = Log::default();
        let mut next = Some(1_000_000_000);
        let samples = vec![0.5f32; 480 * 2];
        // Starts 5 ms before the end of what was delivered: 240 frames trimmed.
        deliver(
            FMT,
            &mut next,
            995_000_000,
            480,
            Some(&samples),
            false,
            &mut log,
        );
        assert_eq!(log.0, [(1_000_000_000, 240, true, false)]);
        // A jump of a minute: passed on as a discontinuity, not filled.
        log.0.clear();
        let at = next.unwrap() + 60_000_000_000;
        deliver(FMT, &mut next, at, 480, Some(&samples), false, &mut log);
        assert_eq!(log.0, [(at, 480, true, true)]);
    }
}
