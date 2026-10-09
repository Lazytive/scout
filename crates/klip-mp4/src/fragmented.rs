//! Fragmented MP4 writer for recordings: `ftyp` + init `moov` (empty sample tables and
//! `mvex`), then one `moof` + `mdat` per fragment. Everything written so far is a playable
//! file, so a recording survives a killed process. [`crate::finalize`] turns the file into a
//! regular MP4 when recording stops.
//!
//! Decode times are rounded from absolute timestamps against one origin (time 0 of every
//! track), so there is no drift however long the recording runs. Each sample's duration is
//! the distance to its successor; callers hold the newest packet of each track back until
//! the next one is known (see [`FragmentPart::next_dts`]).

use std::io::Write;

use klip_media::{Nanos, Packet, TrackInfo};

use crate::boxes::BoxBuf;
use crate::moov::{
    MoovKind, TrackPlan, build_ftyp, build_moov, group_audio_tracks, ns_to_ts, sample_size,
};
use crate::{Mp4Error, Mp4Options, Mp4Track};

/// `trun` sample flags: a sync sample (depends on no other sample).
pub(crate) const SAMPLE_FLAGS_SYNC: u32 = 0x0200_0000;
/// `trun` sample flags: depends on others, not a sync sample.
pub(crate) const SAMPLE_FLAGS_DELTA: u32 = 0x0101_0000;
pub(crate) const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
pub(crate) const TRUN_DATA_OFFSET: u32 = 0x001;
pub(crate) const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x004;
pub(crate) const TRUN_DURATION: u32 = 0x100;
pub(crate) const TRUN_SIZE: u32 = 0x200;
pub(crate) const TRUN_FLAGS: u32 = 0x400;
pub(crate) const TRUN_CTS: u32 = 0x800;

/// The samples of one track in a fragment.
#[derive(Clone, Copy, Debug)]
pub struct FragmentPart<'a> {
    /// Track index (as given to [`FragmentedWriter::new`]).
    pub track: usize,
    /// Packets in decode order; at or after the writer's origin.
    pub packets: &'a [Packet],
    /// Decode time of the packet after the last one, if known: it gives the last sample
    /// its exact duration. Without it the packet's own duration is used.
    pub next_dts: Option<Nanos>,
}

/// Where a fragment went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentInfo {
    /// File offset of the `moof`.
    pub offset: u64,
    /// `moof` + `mdat` bytes.
    pub bytes: u64,
    pub samples: usize,
}

struct TrackState {
    timescale: u32,
    track_id: u32,
    /// Decode time of the last sample written.
    last_dts: Option<Nanos>,
}

/// Writes a fragmented MP4. Not thread-safe; one writer per file. After an error the
/// writer refuses further writes ([`Mp4Error::Poisoned`]): the file may end in a partial
/// fragment, which readers and [`crate::finalize`] ignore.
pub struct FragmentedWriter<W: Write> {
    out: W,
    tracks: Vec<TrackState>,
    origin: Nanos,
    sequence: u32,
    written: u64,
    fragments: u64,
    poisoned: bool,
}

impl<W: Write> std::fmt::Debug for FragmentedWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FragmentedWriter")
            .field("written", &self.written)
            .field("fragments", &self.fragments)
            .finish_non_exhaustive()
    }
}

impl<W: Write> FragmentedWriter<W> {
    /// Write `ftyp` and the init `moov` for `tracks` (their packets are ignored; names and
    /// the enabled flag are used). `origin` is time 0 of every track. With several audio
    /// tracks, players play the enabled one (alternate group, as in clips).
    pub fn new(
        mut out: W,
        tracks: &[Mp4Track<'_>],
        origin: Nanos,
        opts: &Mp4Options<'_>,
    ) -> Result<FragmentedWriter<W>, Mp4Error> {
        if tracks.is_empty() {
            return Err(Mp4Error::NoTracks);
        }
        let mut plans = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| TrackPlan::empty(i, t.info, t.name, t.enabled))
            .collect::<Result<Vec<_>, _>>()?;
        group_audio_tracks(&mut plans);
        let infos: Vec<&TrackInfo> = tracks.iter().map(|t| t.info).collect();
        let ftyp = build_ftyp(&infos, true);
        let moov = build_moov(
            &plans,
            &MoovKind::FragmentInit,
            false,
            opts.creation_time_unix,
        )?;
        out.write_all(&ftyp)?;
        out.write_all(&moov)?;
        out.flush()?;
        let written = u64::try_from(ftyp.len().saturating_add(moov.len()))
            .map_err(|_| Mp4Error::Internal("init size"))?;
        Ok(FragmentedWriter {
            out,
            tracks: plans
                .iter()
                .map(|p| TrackState {
                    timescale: p.timescale,
                    track_id: p.track_id,
                    last_dts: None,
                })
                .collect(),
            origin,
            sequence: 0,
            written,
            fragments: 0,
            poisoned: false,
        })
    }

    /// Bytes written so far (the next fragment's file offset).
    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    pub fn fragments(&self) -> u64 {
        self.fragments
    }

    /// The origin given to [`FragmentedWriter::new`].
    pub fn origin(&self) -> Nanos {
        self.origin
    }

    pub fn get_ref(&self) -> &W {
        &self.out
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    /// Write one fragment (`moof` + `mdat`) holding `parts`, then flush. Parts without
    /// packets are skipped; a fragment with no samples at all writes nothing.
    pub fn write_fragment(&mut self, parts: &[FragmentPart<'_>]) -> Result<FragmentInfo, Mp4Error> {
        if self.poisoned {
            return Err(Mp4Error::Poisoned);
        }
        let (moof, payload_bytes, samples) = self.build_fragment(parts)?;
        if samples == 0 {
            return Ok(FragmentInfo {
                offset: self.written,
                bytes: 0,
                samples: 0,
            });
        }
        let offset = self.written;
        // From here on a failure can leave a partial fragment in the file.
        self.poisoned = true;
        self.out.write_all(&moof)?;
        let mdat_size = payload_bytes
            .checked_add(8)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(Mp4Error::TooLarge("fragment"))?;
        self.out.write_all(&mdat_size.to_be_bytes())?;
        self.out.write_all(b"mdat")?;
        for part in parts {
            for p in part.packets {
                self.out.write_all(&p.data)?;
            }
        }
        self.out.flush()?;
        self.poisoned = false;
        for part in parts {
            if let (Some(t), Some(last)) = (self.tracks.get_mut(part.track), part.packets.last()) {
                t.last_dts = Some(last.dts);
            }
        }
        let bytes = u64::try_from(moof.len())
            .map_err(|_| Mp4Error::Internal("moof size"))?
            .saturating_add(u64::from(mdat_size));
        self.written = self.written.saturating_add(bytes);
        self.fragments = self.fragments.saturating_add(1);
        Ok(FragmentInfo {
            offset,
            bytes,
            samples,
        })
    }

    /// Build the `moof` (data offsets patched) and validate every part. Nothing is written.
    fn build_fragment(
        &mut self,
        parts: &[FragmentPart<'_>],
    ) -> Result<(Vec<u8>, u64, usize), Mp4Error> {
        let mut seen = vec![false; self.tracks.len()];
        let mut b = BoxBuf::new();
        let moof = b.start(b"moof");
        let mfhd = b.start_full(b"mfhd", 0, 0);
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(Mp4Error::TooLarge("fragment count"))?;
        b.u32(sequence);
        b.finish(mfhd)?;
        // (position of the data_offset field, payload offset of the part within the mdat)
        let mut patches: Vec<(usize, u64)> = Vec::new();
        let mut payload: u64 = 0;
        let mut samples: usize = 0;
        for part in parts.iter().filter(|p| !p.packets.is_empty()) {
            let state = self
                .tracks
                .get(part.track)
                .ok_or_else(|| Mp4Error::InvalidTrack(part.track, "no such track".into()))?;
            let slot = seen
                .get_mut(part.track)
                .ok_or(Mp4Error::Internal("track index"))?;
            if *slot {
                return Err(Mp4Error::InvalidTrack(
                    part.track,
                    "twice in one fragment".into(),
                ));
            }
            *slot = true;
            let timing = part_timing(part, state, self.origin)?;
            let traf = b.start(b"traf");
            let at = b.start_full(b"tfhd", 0, TFHD_DEFAULT_BASE_IS_MOOF);
            b.u32(state.track_id);
            b.finish(at)?;
            let at = b.start_full(b"tfdt", 1, 0);
            b.u64(timing.base);
            b.finish(at)?;
            let with_cts = timing.cts.iter().any(|c| *c != 0);
            let mut flags = TRUN_DATA_OFFSET | TRUN_DURATION | TRUN_SIZE | TRUN_FLAGS;
            if with_cts {
                flags |= TRUN_CTS;
            }
            let at = b.start_full(b"trun", u8::from(with_cts), flags);
            b.u32(u32::try_from(part.packets.len()).map_err(|_| Mp4Error::TooLarge("samples"))?);
            patches.push((b.buf.len(), payload));
            b.i32(0); // data_offset, patched below
            for (i, p) in part.packets.iter().enumerate() {
                let size = sample_size(p)?;
                b.u32(timing.durations.get(i).copied().unwrap_or(0));
                b.u32(size);
                b.u32(if p.keyframe {
                    SAMPLE_FLAGS_SYNC
                } else {
                    SAMPLE_FLAGS_DELTA
                });
                if with_cts {
                    b.i32(timing.cts.get(i).copied().unwrap_or(0));
                }
                payload = payload
                    .checked_add(u64::from(size))
                    .ok_or(Mp4Error::TooLarge("fragment"))?;
            }
            b.finish(at)?;
            b.finish(traf)?;
            samples = samples.saturating_add(part.packets.len());
        }
        b.finish(moof)?;
        let moof_len = u64::try_from(b.buf.len()).map_err(|_| Mp4Error::Internal("moof size"))?;
        for (pos, part_offset) in patches {
            // Relative to the moof start (default-base-is-moof): moof, mdat header, data.
            let off = moof_len
                .checked_add(8)
                .and_then(|v| v.checked_add(part_offset))
                .and_then(|v| i32::try_from(v).ok())
                .ok_or(Mp4Error::TooLarge("fragment"))?;
            let end = pos
                .checked_add(4)
                .ok_or(Mp4Error::Internal("patch offset"))?;
            b.buf
                .get_mut(pos..end)
                .ok_or(Mp4Error::Internal("patch offset"))?
                .copy_from_slice(&off.to_be_bytes());
        }
        if samples > 0 {
            self.sequence = sequence;
        }
        Ok((b.buf, payload, samples))
    }
}

struct PartTiming {
    /// `tfdt` base media decode time.
    base: u64,
    durations: Vec<u32>,
    cts: Vec<i32>,
}

/// Decode times, durations and composition offsets of one part, in the track timescale.
fn part_timing(
    part: &FragmentPart<'_>,
    state: &TrackState,
    origin: Nanos,
) -> Result<PartTiming, Mp4Error> {
    let bad = |what: &str| Mp4Error::InvalidTrack(part.track, what.into());
    let ts = state.timescale;
    let decode = |dts: Nanos| -> Result<i64, Mp4Error> {
        let rel = dts
            .checked_sub(origin)
            .ok_or(Mp4Error::TooLarge("timestamp"))?;
        if rel < 0 {
            return Err(bad("packet before the recording's origin"));
        }
        Ok(ns_to_ts(rel, ts))
    };
    let mut prev = state.last_dts;
    let mut times = Vec::with_capacity(part.packets.len().saturating_add(1));
    let mut cts = Vec::with_capacity(part.packets.len());
    for p in part.packets {
        if prev.is_some_and(|last| p.dts < last) {
            return Err(Mp4Error::NonMonotonic(part.track));
        }
        prev = Some(p.dts);
        times.push(decode(p.dts)?);
        let off = ns_to_ts(p.pts.saturating_sub(p.dts), ts);
        cts.push(i32::try_from(off).map_err(|_| Mp4Error::TooLarge("composition offset"))?);
    }
    let last = part
        .packets
        .last()
        .ok_or(Mp4Error::Internal("empty part"))?;
    let end = match part.next_dts {
        Some(next) if next >= last.dts => decode(next)?,
        Some(_) => return Err(Mp4Error::NonMonotonic(part.track)),
        None => decode(last.dts.saturating_add(last.duration.max(0)))?,
    };
    times.push(end);
    let mut durations = Vec::with_capacity(part.packets.len());
    for pair in times.windows(2) {
        let (a, b) = match pair {
            [a, b] => (*a, *b),
            _ => return Err(Mp4Error::Internal("window size")),
        };
        let d = b
            .checked_sub(a)
            .ok_or(Mp4Error::TooLarge("sample duration"))?;
        durations.push(u32::try_from(d).map_err(|_| Mp4Error::TooLarge("sample duration"))?);
    }
    // Without a successor or a duration, the last sample repeats the previous duration.
    if part.next_dts.is_none()
        && last.duration <= 0
        && let (Some(prev), Some(slot)) = (
            durations
                .len()
                .checked_sub(2)
                .and_then(|i| durations.get(i))
                .copied(),
            durations.last_mut(),
        )
    {
        *slot = prev;
    }
    let base = times
        .first()
        .and_then(|t| u64::try_from(*t).ok())
        .ok_or(Mp4Error::Internal("decode time"))?;
    Ok(PartTiming {
        base,
        durations,
        cts,
    })
}
