//! MP4 writing for packets that are already encoded (no re-encode).
//!
//! - Clips: faststart files. All sample sizes and timestamps are known up front, so the
//!   `moov` box is computed first and the file is written in one sequential pass: `ftyp`,
//!   `moov`, `mdat`. Samples from all tracks are interleaved in decode-time order, one
//!   sample per chunk. See `docs/adr/0003-own-mp4-writer.md`.
//! - Recordings: fragmented while recording ([`FragmentedWriter`]), so a killed process
//!   leaves a playable file, and turned into a regular MP4 on stop ([`finalize`]).
//!   See `docs/adr/0013-full-session-recording.md`.
//! - Reading: [`probe`] summarises any MP4 for the library (ADR 0015);
//!   [`scan_fragmented`] reads recordings back for finalising.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod boxes;
mod finalize;
mod fragmented;
mod info;
mod moov;
mod read;

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use klip_media::{Nanos, Packet, TrackInfo};

pub use crate::finalize::{FinalizeSummary, finalize};
pub use crate::fragmented::{FragmentInfo, FragmentPart, FragmentedWriter};
pub use crate::info::{MAX_INFO_MOOV_BYTES, Mp4Info, VideoSummary, probe};
use crate::moov::{Chapter, MoovKind, TrackPlan, build_ftyp, build_moov, group_audio_tracks};
pub use crate::read::{FragmentedScan, ScannedSample, ScannedTrack, scan_fragmented};

/// Errors from building or writing an MP4.
#[derive(Debug, thiserror::Error)]
pub enum Mp4Error {
    #[error("no tracks to write")]
    NoTracks,
    #[error("track {0} has no packets")]
    EmptyTrack(usize),
    #[error("track {0} is invalid: {1}")]
    InvalidTrack(usize, String),
    #[error("track {0}: decode timestamps go backwards")]
    NonMonotonic(usize),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("value too large for MP4: {0}")]
    TooLarge(&'static str),
    #[error("internal error: {0}")]
    Internal(&'static str),
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
    #[error("not a fragmented Scout recording: {0}")]
    NotFragmented(&'static str),
    #[error("malformed file: {0}")]
    Malformed(String),
    #[error("the recording has no complete fragment")]
    NoFragments,
    #[error("the writer failed earlier and can't be used")]
    Poisoned,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// One track to write.
#[derive(Clone, Copy, Debug)]
pub struct Mp4Track<'a> {
    pub info: &'a TrackInfo,
    /// Packets in decode order.
    pub packets: &'a [Packet],
    /// Track name (`hdlr` name; ffprobe shows it as `handler_name`).
    pub name: Option<&'a str>,
    /// Played by default. Players play the enabled track of each kind; editors import
    /// all tracks. When a file has several audio tracks they form one alternate group,
    /// so players pick one (the enabled one) instead of mixing them.
    pub enabled: bool,
}

impl<'a> Mp4Track<'a> {
    /// An enabled, unnamed track.
    pub fn new(info: &'a TrackInfo, packets: &'a [Packet]) -> Mp4Track<'a> {
        Mp4Track {
            info,
            packets,
            name: None,
            enabled: true,
        }
    }

    pub fn named(self, name: &'a str) -> Mp4Track<'a> {
        Mp4Track {
            name: Some(name),
            ..self
        }
    }

    pub fn enabled(self, enabled: bool) -> Mp4Track<'a> {
        Mp4Track { enabled, ..self }
    }
}

/// Longest track name written (bytes); longer names are cut at a character boundary.
pub const MAX_TRACK_NAME_BYTES: usize = 255;

/// A point in time to mark (a bookmark). Written as a chapter start in `udta/chpl`, which
/// ffmpeg/ffprobe, VLC and mpv show as chapters; at most 255 per file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    /// Time on the same timeline as the packet timestamps ([`write_mp4`]) or relative to
    /// the recording's origin ([`finalize`]).
    pub at: Nanos,
    pub title: String,
}

/// File-level options.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mp4Options<'a> {
    /// Creation time, seconds since the Unix epoch.
    pub creation_time_unix: u64,
    /// Force 64-bit chunk offsets (normally chosen automatically above 4 GiB).
    pub force_co64: bool,
    /// Markers; those outside the written range are left out.
    pub markers: &'a [Marker],
}

/// Chapters for markers on a timeline whose movie time 0 is `origin`, up to `duration`.
pub(crate) fn chapters(markers: &[Marker], origin: Nanos, duration: Nanos) -> Vec<Chapter<'_>> {
    let mut out: Vec<Chapter<'_>> = markers
        .iter()
        .filter_map(|m| {
            let rel = m.at.checked_sub(origin)?;
            if rel < 0 || rel > duration {
                return None;
            }
            Some(Chapter {
                at_100ns: u64::try_from(rel / 100).ok()?,
                title: &m.title,
            })
        })
        .collect();
    out.sort_by_key(|c| c.at_100ns);
    out
}

/// What was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mp4Summary {
    pub bytes: u64,
    /// Longest track presentation duration.
    pub duration: Nanos,
    pub samples: usize,
}

/// A sample whose bytes live outside the packet (in a spool file): what
/// [`write_mp4_spooled`] needs to know about each packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub pts: Nanos,
    pub dts: Nanos,
    /// Duration, or 0 when unknown.
    pub duration: Nanos,
    pub keyframe: bool,
    pub size: u32,
    /// Where the bytes start in the spool.
    pub offset: u64,
}

impl Sample {
    /// The metadata of a packet (`offset` 0).
    pub fn of(p: &Packet) -> Result<Sample, Mp4Error> {
        Ok(Sample {
            pts: p.pts,
            dts: p.dts,
            duration: p.duration,
            keyframe: p.keyframe,
            size: u32::try_from(p.data.len()).map_err(|_| Mp4Error::TooLarge("sample size"))?,
            offset: 0,
        })
    }
}

/// A track whose sample bytes sit in a spool (see [`write_mp4_spooled`]).
#[derive(Clone, Copy, Debug)]
pub struct SpooledTrack<'a> {
    pub info: &'a TrackInfo,
    /// Samples in decode order.
    pub samples: &'a [Sample],
    pub name: Option<&'a str>,
    pub enabled: bool,
}

/// What both writers work from.
struct Desc<'a> {
    info: &'a TrackInfo,
    name: Option<&'a str>,
    enabled: bool,
    samples: Vec<Sample>,
}

/// Write an MP4 to `out`. Tracks keep their order; give video first.
pub fn write_mp4<W: Write>(
    out: &mut W,
    tracks: &[Mp4Track<'_>],
    opts: &Mp4Options<'_>,
) -> Result<Mp4Summary, Mp4Error> {
    let descs = tracks
        .iter()
        .map(|t| {
            Ok(Desc {
                info: t.info,
                name: t.name,
                enabled: t.enabled,
                samples: t.packets.iter().map(Sample::of).collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<Vec<_>, Mp4Error>>()?;
    write_inner(out, &descs, opts, |w, ti, si, _| {
        let p = tracks
            .get(ti)
            .and_then(|t| t.packets.get(si))
            .ok_or(Mp4Error::Internal("sample index"))?;
        w.write_all(&p.data)?;
        Ok(())
    })
}

/// [`write_mp4`] for packets whose bytes were spooled to `payload` as they were
/// produced (an export that must not hold the whole file in memory): each sample is
/// read back from its `offset`. The result is byte for byte what `write_mp4` writes.
pub fn write_mp4_spooled<W: Write, R: Read + Seek>(
    out: &mut W,
    tracks: &[SpooledTrack<'_>],
    opts: &Mp4Options<'_>,
    payload: &mut R,
) -> Result<Mp4Summary, Mp4Error> {
    let descs: Vec<Desc<'_>> = tracks
        .iter()
        .map(|t| Desc {
            info: t.info,
            name: t.name,
            enabled: t.enabled,
            samples: t.samples.to_vec(),
        })
        .collect();
    write_inner(out, &descs, opts, |w, _, _, s| {
        payload.seek(SeekFrom::Start(s.offset))?;
        let copied = std::io::copy(&mut payload.take(u64::from(s.size)), w)?;
        if copied != u64::from(s.size) {
            return Err(Mp4Error::Malformed("the spool is short".into()));
        }
        Ok(())
    })
}

fn write_inner<W: Write>(
    out: &mut W,
    tracks: &[Desc<'_>],
    opts: &Mp4Options<'_>,
    mut emit: impl FnMut(&mut W, usize, usize, &Sample) -> Result<(), Mp4Error>,
) -> Result<Mp4Summary, Mp4Error> {
    if tracks.is_empty() {
        return Err(Mp4Error::NoTracks);
    }
    let mut plans = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| TrackPlan::from_samples(i, t.info, t.name, t.enabled, &t.samples))
        .collect::<Result<Vec<_>, _>>()?;
    group_audio_tracks(&mut plans);

    // Align tracks on the earliest presentation time.
    let origin = tracks
        .iter()
        .filter_map(|t| t.samples.first().map(|p| p.pts))
        .min()
        .ok_or(Mp4Error::NoTracks)?;
    for (plan, t) in plans.iter_mut().zip(tracks) {
        let first_pts = t.samples.first().map(|p| p.pts).unwrap_or(origin);
        plan.empty_edit = first_pts.saturating_sub(origin).max(0);
    }
    let duration = plans.iter().map(TrackPlan::total_ns).max().unwrap_or(0);
    let chapters = chapters(opts.markers, origin, duration);

    // Interleave samples by decode time (stable on track order).
    let mut order: Vec<(Nanos, usize, usize)> = Vec::new();
    for (ti, t) in tracks.iter().enumerate() {
        for (si, p) in t.samples.iter().enumerate() {
            order.push((p.dts, ti, si));
        }
    }
    order.sort_by_key(|&(dts, ti, si)| (dts, ti, si));

    let data_size = tracks
        .iter()
        .flat_map(|t| t.samples.iter())
        .try_fold(0u64, |acc, p| acc.checked_add(u64::from(p.size)))
        .ok_or(Mp4Error::TooLarge("media data"))?;
    let infos: Vec<&TrackInfo> = tracks.iter().map(|t| t.info).collect();
    let ftyp = build_ftyp(&infos, false);
    let ftyp_len = u64::try_from(ftyp.len()).map_err(|_| Mp4Error::Internal("ftyp size"))?;
    let mdat_header_len: u64 = if data_size > u64::from(u32::MAX).saturating_sub(8) {
        16
    } else {
        8
    };
    let kind = MoovKind::Regular {
        chapters: &chapters,
    };

    // First pass with 32-bit offsets (unless forced) to learn the moov size.
    let mut use_co64 = opts.force_co64;
    let moov = loop {
        layout_chunks(&mut plans, tracks, &order, 0)?;
        let probe = build_moov(&plans, &kind, use_co64, opts.creation_time_unix)?;
        let moov_len = u64::try_from(probe.len()).map_err(|_| Mp4Error::Internal("moov size"))?;
        let data_start = ftyp_len
            .checked_add(moov_len)
            .and_then(|v| v.checked_add(mdat_header_len))
            .ok_or(Mp4Error::TooLarge("file"))?;
        let data_end = data_start
            .checked_add(data_size)
            .ok_or(Mp4Error::TooLarge("file"))?;
        if !use_co64 && data_end > u64::from(u32::MAX) {
            use_co64 = true;
            continue;
        }
        layout_chunks(&mut plans, tracks, &order, data_start)?;
        let real = build_moov(&plans, &kind, use_co64, opts.creation_time_unix)?;
        if real.len() != probe.len() {
            return Err(Mp4Error::Internal("moov size changed between passes"));
        }
        break real;
    };

    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    if mdat_header_len == 16 {
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&data_size.saturating_add(16).to_be_bytes())?;
    } else {
        let size =
            u32::try_from(data_size.saturating_add(8)).map_err(|_| Mp4Error::TooLarge("mdat"))?;
        out.write_all(&size.to_be_bytes())?;
        out.write_all(b"mdat")?;
    }
    for &(_, ti, si) in &order {
        let s = tracks
            .get(ti)
            .and_then(|t| t.samples.get(si))
            .ok_or(Mp4Error::Internal("sample index"))?;
        emit(out, ti, si, s)?;
    }
    out.flush()?;

    let bytes = ftyp_len
        .saturating_add(u64::try_from(moov.len()).unwrap_or(u64::MAX))
        .saturating_add(mdat_header_len)
        .saturating_add(data_size);
    Ok(Mp4Summary {
        bytes,
        duration,
        samples: order.len(),
    })
}

/// One sample per chunk: chunk offsets follow the interleave order from `data_start`.
fn layout_chunks(
    plans: &mut [TrackPlan<'_>],
    tracks: &[Desc<'_>],
    order: &[(Nanos, usize, usize)],
    data_start: u64,
) -> Result<(), Mp4Error> {
    for (plan, t) in plans.iter_mut().zip(tracks) {
        plan.chunks = vec![(0, 1); t.samples.len()];
    }
    let mut pos = data_start;
    for &(_, ti, si) in order {
        let size = tracks
            .get(ti)
            .and_then(|t| t.samples.get(si))
            .map(|p| p.size)
            .ok_or(Mp4Error::Internal("sample index"))?;
        let slot = plans
            .get_mut(ti)
            .and_then(|p| p.chunks.get_mut(si))
            .ok_or(Mp4Error::Internal("chunk index"))?;
        slot.0 = pos;
        pos = pos
            .checked_add(u64::from(size))
            .ok_or(Mp4Error::TooLarge("file"))?;
    }
    Ok(())
}

/// Write an MP4 atomically: to `<path>.part` in the same folder, fsync, then rename.
/// Refuses to overwrite an existing file. The temp file is removed on failure.
pub fn save_mp4_atomic(
    path: &Path,
    tracks: &[Mp4Track<'_>],
    opts: &Mp4Options<'_>,
) -> Result<Mp4Summary, Mp4Error> {
    if path.exists() {
        return Err(Mp4Error::AlreadyExists(path.to_path_buf()));
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".part");
    let tmp = PathBuf::from(tmp_name);
    let result = (|| {
        let file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        let summary = write_mp4(&mut w, tracks, opts)?;
        let file: File = w.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        if path.exists() {
            return Err(Mp4Error::AlreadyExists(path.to_path_buf()));
        }
        fs::rename(&tmp, path)?;
        Ok(summary)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod frag_tests;
