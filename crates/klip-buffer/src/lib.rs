//! Replay buffer: a ring of **encoded** packets per track, bounded by duration and bytes.
//!
//! One video track is the *anchor*. Eviction drops whole GOPs from the front of the
//! anchor so the oldest anchor packet is always a keyframe; other tracks are trimmed to
//! the anchor's start. [`Ring::snapshot`] returns the packets of the last `N` seconds,
//! starting at the keyframe at or before the requested start, ready to be remuxed
//! without re-encoding. Packet payloads are [`bytes::Bytes`], so a snapshot is cheap
//! (reference counts, no copies) and can be written to disk outside the lock.
//!
//! All bookkeeping uses decode timestamps, which are monotonic per track.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

use std::collections::VecDeque;

use klip_media::{MAX_PACKET_BYTES, Nanos, Packet, TrackInfo, TrackKind};

/// Keyframes this close after a snapshot's start target are treated as at the target.
pub const SNAP_TOLERANCE: Nanos = 1_000_000;

/// Limits for the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingConfig {
    /// Keep at least this much history (rounded up to the next GOP boundary).
    pub max_duration: Nanos,
    /// Hard cap on the sum of payload bytes across all tracks.
    pub max_bytes: u64,
}

/// Why a packet or track description was not accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushError {
    NoSuchTrack(usize),
    /// The track has no [`TrackInfo`] yet, so its packets couldn't be decoded later.
    TrackNotReady(usize),
    PacketTooLarge(usize),
    /// Decode timestamps must not go backwards within a track.
    NonMonotonicDts {
        last: Nanos,
        got: Nanos,
    },
    /// The anchor track must start with a keyframe; leading delta frames are dropped.
    WaitingForKeyframe,
    KindMismatch,
    InvalidTrackInfo(String),
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PushError::NoSuchTrack(t) => write!(f, "no track {t}"),
            PushError::TrackNotReady(t) => write!(f, "track {t} has no codec description yet"),
            PushError::PacketTooLarge(n) => write!(f, "packet too large ({n} bytes)"),
            PushError::NonMonotonicDts { last, got } => {
                write!(f, "decode timestamp went backwards ({got} < {last})")
            }
            PushError::WaitingForKeyframe => write!(f, "waiting for a keyframe"),
            PushError::KindMismatch => write!(f, "track description has the wrong kind"),
            PushError::InvalidTrackInfo(e) => write!(f, "invalid track description: {e}"),
        }
    }
}

impl std::error::Error for PushError {}

/// Why a snapshot couldn't be taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// Nothing decodable has been buffered yet.
    Empty,
    InvalidLength,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Empty => write!(f, "the replay buffer is empty"),
            SnapshotError::InvalidLength => write!(f, "clip length must be positive"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// The packets of one track in a snapshot.
#[derive(Clone, Debug)]
pub struct TrackSnapshot {
    /// Index of the track in the ring.
    pub track: usize,
    pub info: TrackInfo,
    pub packets: Vec<Packet>,
}

/// A consistent copy of the last part of the ring.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Same order as the ring's tracks; tracks without info or packets are omitted.
    pub tracks: Vec<TrackSnapshot>,
    /// DTS of the first anchor packet (a keyframe).
    pub start: Nanos,
    /// End of the last anchor packet.
    pub end: Nanos,
}

impl Snapshot {
    /// Duration covered by the anchor track.
    pub fn duration(&self) -> Nanos {
        self.end.saturating_sub(self.start)
    }
}

/// Counters for status displays and tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RingStats {
    /// Span of the anchor track (first DTS to end of last packet).
    pub buffered: Nanos,
    pub bytes: u64,
    pub packets: Vec<usize>,
    pub keyframes: usize,
    /// Packets refused by [`Ring::push`] since creation.
    pub rejected: u64,
    /// Packets dropped by eviction since creation.
    pub evicted: u64,
}

#[derive(Debug)]
struct TrackRing {
    kind: TrackKind,
    info: Option<TrackInfo>,
    packets: VecDeque<Packet>,
    bytes: u64,
    last_dts: Option<Nanos>,
}

impl TrackRing {
    fn clear(&mut self) {
        self.packets.clear();
        self.bytes = 0;
        self.last_dts = None;
    }

    fn pop_front(&mut self) -> Option<Packet> {
        let p = self.packets.pop_front()?;
        self.bytes = self.bytes.saturating_sub(len_u64(&p));
        Some(p)
    }

    fn first_dts(&self) -> Option<Nanos> {
        self.packets.front().map(|p| p.dts)
    }

    fn end(&self) -> Option<Nanos> {
        self.packets.back().map(Packet::dts_end)
    }
}

/// The replay ring. Not thread-safe by itself; wrap it in a mutex.
#[derive(Debug)]
pub struct Ring {
    cfg: RingConfig,
    tracks: Vec<TrackRing>,
    anchor: usize,
    rejected: u64,
    evicted: u64,
}

fn len_u64(p: &Packet) -> u64 {
    u64::try_from(p.data.len()).unwrap_or(u64::MAX)
}

impl Ring {
    /// Create a ring for tracks of the given kinds. The first video track is the anchor;
    /// returns `None` if there is no video track.
    pub fn new(cfg: RingConfig, kinds: &[TrackKind]) -> Option<Ring> {
        let anchor = kinds.iter().position(|k| *k == TrackKind::Video)?;
        let tracks = kinds
            .iter()
            .map(|&kind| TrackRing {
                kind,
                info: None,
                packets: VecDeque::new(),
                bytes: 0,
                last_dts: None,
            })
            .collect();
        Some(Ring {
            cfg,
            tracks,
            anchor,
            rejected: 0,
            evicted: 0,
        })
    }

    /// The ring's limits.
    pub fn config(&self) -> RingConfig {
        self.cfg
    }

    /// Change the limits; evicts immediately if they shrank.
    pub fn set_config(&mut self, cfg: RingConfig) {
        self.cfg = cfg;
        self.evict();
    }

    /// Set or replace a track's codec description. If it differs from the current one
    /// (for example new SPS/PPS after a resolution change) the track's buffered packets
    /// are dropped, because they can't be muxed under the new description.
    pub fn set_track_info(&mut self, track: usize, info: TrackInfo) -> Result<(), PushError> {
        info.validate()
            .map_err(|e| PushError::InvalidTrackInfo(e.to_string()))?;
        let anchor = self.anchor;
        let t = self
            .tracks
            .get_mut(track)
            .ok_or(PushError::NoSuchTrack(track))?;
        if info.kind() != t.kind {
            return Err(PushError::KindMismatch);
        }
        if t.info.as_ref() != Some(&info) {
            t.clear();
            t.info = Some(info);
            if track == anchor {
                self.evict();
            }
        }
        Ok(())
    }

    /// Append one packet. Errors leave the ring unchanged (apart from the reject counter).
    pub fn push(&mut self, track: usize, packet: Packet) -> Result<(), PushError> {
        let res = self.try_push(track, packet);
        if res.is_err() {
            self.rejected = self.rejected.saturating_add(1);
        }
        res
    }

    fn try_push(&mut self, track: usize, packet: Packet) -> Result<(), PushError> {
        if packet.data.len() > MAX_PACKET_BYTES {
            return Err(PushError::PacketTooLarge(packet.data.len()));
        }
        let is_anchor = track == self.anchor;
        let t = self
            .tracks
            .get_mut(track)
            .ok_or(PushError::NoSuchTrack(track))?;
        if t.info.is_none() {
            return Err(PushError::TrackNotReady(track));
        }
        if let Some(last) = t.last_dts
            && packet.dts < last
        {
            return Err(PushError::NonMonotonicDts {
                last,
                got: packet.dts,
            });
        }
        if is_anchor && t.packets.is_empty() && !packet.keyframe {
            return Err(PushError::WaitingForKeyframe);
        }
        t.last_dts = Some(packet.dts);
        t.bytes = t.bytes.saturating_add(len_u64(&packet));
        t.packets.push_back(packet);
        self.evict();
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.tracks
            .iter()
            .fold(0u64, |acc, t| acc.saturating_add(t.bytes))
    }

    fn anchor_span(&self) -> Nanos {
        let Some(a) = self.tracks.get(self.anchor) else {
            return 0;
        };
        match (a.first_dts(), a.end()) {
            (Some(s), Some(e)) => e.saturating_sub(s),
            _ => 0,
        }
    }

    /// Drop the oldest anchor GOP. Returns false if nothing could be dropped.
    fn drop_oldest_gop(&mut self) -> bool {
        let Some(a) = self.tracks.get_mut(self.anchor) else {
            return false;
        };
        // Index of the second keyframe: everything before it is the oldest GOP.
        let next_key = a
            .packets
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, p)| p.keyframe)
            .map(|(i, _)| i);
        let n = match next_key {
            Some(i) => i,
            // A single GOP: only drop it if the byte cap forces us to.
            None => a.packets.len(),
        };
        if n == 0 {
            return false;
        }
        let mut dropped = 0u64;
        for _ in 0..n {
            if a.pop_front().is_some() {
                dropped = dropped.saturating_add(1);
            }
        }
        self.evicted = self.evicted.saturating_add(dropped);
        dropped > 0
    }

    fn evict(&mut self) {
        // Duration bound: drop GOPs while the remainder still covers max_duration.
        while let Some(a) = self.tracks.get(self.anchor) {
            let next_key_dts = a.packets.iter().skip(1).find(|p| p.keyframe).map(|p| p.dts);
            let (Some(next), Some(end)) = (next_key_dts, a.end()) else {
                break;
            };
            if end.saturating_sub(next) < self.cfg.max_duration || !self.drop_oldest_gop() {
                break;
            }
        }
        // Byte bound: hard cap.
        while self.total_bytes() > self.cfg.max_bytes {
            let anchor_has_packets = self
                .tracks
                .get(self.anchor)
                .is_some_and(|a| !a.packets.is_empty());
            if anchor_has_packets {
                if !self.drop_oldest_gop() {
                    break;
                }
            } else if !self.drop_oldest_secondary() {
                break;
            }
            self.trim_secondary_tracks();
        }
        self.trim_secondary_tracks();
    }

    /// Drop the single oldest packet of any non-anchor track (used only when the anchor is
    /// empty and the byte cap is exceeded).
    fn drop_oldest_secondary(&mut self) -> bool {
        let anchor = self.anchor;
        let oldest = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != anchor)
            .filter_map(|(i, t)| t.first_dts().map(|d| (i, d)))
            .min_by_key(|(_, d)| *d)
            .map(|(i, _)| i);
        let Some(i) = oldest else {
            return false;
        };
        let popped = self.tracks.get_mut(i).and_then(TrackRing::pop_front);
        if popped.is_some() {
            self.evicted = self.evicted.saturating_add(1);
        }
        popped.is_some()
    }

    /// Non-anchor tracks never hold data older than the anchor's first packet, or (when
    /// the anchor is empty) older than `max_duration` before their own newest packet.
    fn trim_secondary_tracks(&mut self) {
        let anchor_start = self.tracks.get(self.anchor).and_then(TrackRing::first_dts);
        let max = self.cfg.max_duration;
        let anchor = self.anchor;
        let mut evicted = 0u64;
        for (i, t) in self.tracks.iter_mut().enumerate() {
            if i == anchor {
                continue;
            }
            let cutoff = match anchor_start {
                Some(s) => s,
                None => match t.end() {
                    Some(e) => e.saturating_sub(max),
                    None => continue,
                },
            };
            while t.packets.front().is_some_and(|p| p.dts < cutoff) {
                if t.pop_front().is_some() {
                    evicted = evicted.saturating_add(1);
                }
            }
        }
        self.evicted = self.evicted.saturating_add(evicted);
    }

    /// End (last DTS + duration) of track `track`, if it has packets.
    pub fn track_end(&self, track: usize) -> Option<Nanos> {
        self.tracks.get(track).and_then(TrackRing::end)
    }

    /// End of the anchor track.
    pub fn anchor_end(&self) -> Option<Nanos> {
        self.track_end(self.anchor)
    }

    /// Indices of non-anchor tracks that have a description (i.e. are producing data).
    pub fn active_secondary_tracks(&self) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(i, t)| *i != self.anchor && t.info.is_some())
            .map(|(i, _)| i)
            .collect()
    }

    /// Copy out the last `length` of the buffer, starting at the anchor keyframe at or
    /// before `end - length` (or the oldest keyframe if the buffer is shorter).
    pub fn snapshot(&self, length: Nanos) -> Result<Snapshot, SnapshotError> {
        self.snapshot_until(None, length)
    }

    /// Like [`Ring::snapshot`], but ignoring anchor packets that start at or after
    /// `until`: the clip ends where it was requested even if saving waited for other
    /// tracks to catch up.
    pub fn snapshot_until(
        &self,
        until: Option<Nanos>,
        length: Nanos,
    ) -> Result<Snapshot, SnapshotError> {
        if length <= 0 {
            return Err(SnapshotError::InvalidLength);
        }
        let a = self.tracks.get(self.anchor).ok_or(SnapshotError::Empty)?;
        let limit = until.unwrap_or(Nanos::MAX);
        let last = a.packets.iter().rev().find(|p| p.dts < limit);
        let (Some(info), Some(end)) = (a.info.as_ref(), last.map(Packet::dts_end)) else {
            return Err(SnapshotError::Empty);
        };
        // A keyframe up to SNAP_TOLERANCE after the target counts as "at" it, so timestamp
        // rounding (e.g. 1/60 s truncated to whole ns) can't add a whole extra GOP.
        let target = end.saturating_sub(length).saturating_add(SNAP_TOLERANCE);
        // Latest keyframe with dts <= target; else the first packet (always a keyframe).
        let start_idx = a
            .packets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.keyframe && p.dts <= target)
            .map(|(i, _)| i)
            .next_back()
            .unwrap_or(0);
        let anchor_packets: Vec<Packet> = a
            .packets
            .iter()
            .skip(start_idx)
            .take_while(|p| p.dts < limit)
            .cloned()
            .collect();
        let Some(start) = anchor_packets.first().map(|p| p.dts) else {
            return Err(SnapshotError::Empty);
        };
        let mut tracks = Vec::with_capacity(self.tracks.len());
        for (i, t) in self.tracks.iter().enumerate() {
            if i == self.anchor {
                tracks.push(TrackSnapshot {
                    track: i,
                    info: info.clone(),
                    packets: anchor_packets.clone(),
                });
                continue;
            }
            let Some(tinfo) = t.info.as_ref() else {
                continue;
            };
            let packets: Vec<Packet> = t
                .packets
                .iter()
                .filter(|p| p.pts >= start && p.pts < end)
                .cloned()
                .collect();
            if !packets.is_empty() {
                tracks.push(TrackSnapshot {
                    track: i,
                    info: tinfo.clone(),
                    packets,
                });
            }
        }
        Ok(Snapshot { tracks, start, end })
    }

    /// Every described track with its packets from the newest anchor keyframe on (other
    /// tracks: packets whose dts is at or after that keyframe's). Packets may be empty,
    /// e.g. all of them while the anchor has no keyframe yet. Used to start a recording
    /// with what is already buffered.
    pub fn tail_from_last_keyframe(&self) -> Vec<TrackSnapshot> {
        let start = self
            .tracks
            .get(self.anchor)
            .and_then(|a| a.packets.iter().rev().find(|p| p.keyframe))
            .map(|p| p.dts);
        self.tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let info = t.info.clone()?;
                let packets = match start {
                    Some(s) => t.packets.iter().filter(|p| p.dts >= s).cloned().collect(),
                    None => Vec::new(),
                };
                Some(TrackSnapshot {
                    track: i,
                    info,
                    packets,
                })
            })
            .collect()
    }

    /// Current counters.
    pub fn stats(&self) -> RingStats {
        RingStats {
            buffered: self.anchor_span(),
            bytes: self.total_bytes(),
            packets: self.tracks.iter().map(|t| t.packets.len()).collect(),
            keyframes: self
                .tracks
                .get(self.anchor)
                .map(|a| a.packets.iter().filter(|p| p.keyframe).count())
                .unwrap_or(0),
            rejected: self.rejected,
            evicted: self.evicted,
        }
    }

    /// Drop everything (keeps track descriptions).
    pub fn clear(&mut self) {
        for t in &mut self.tracks {
            t.clear();
        }
    }
}

#[cfg(test)]
mod tests;
