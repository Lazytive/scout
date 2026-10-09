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
/// Overlap with already delivered audio that gets trimmed.
pub(crate) const OVERLAP_NS: i64 = 1_000_000;

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
