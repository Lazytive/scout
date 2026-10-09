//! Faststart MP4 writer for packets that are already encoded.
//!
//! All sample sizes and timestamps are known up front, so the `moov` box is computed
//! first and the file is written in one sequential pass: `ftyp`, `moov`, `mdat`. Samples
//! from all tracks are interleaved in decode-time order, one sample per chunk.
//! See `docs/adr/0003-own-mp4-writer.md`.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod boxes;

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use klip_media::{Codec, NANOS_PER_SEC, Nanos, Packet, TrackInfo, TrackParams};

use crate::boxes::BoxBuf;

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
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// One track to write.
#[derive(Clone, Copy, Debug)]
pub struct Mp4Track<'a> {
    pub info: &'a TrackInfo,
    /// Packets in decode order.
    pub packets: &'a [Packet],
}

/// File-level options.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mp4Options {
    /// Creation time, seconds since the Unix epoch.
    pub creation_time_unix: u64,
    /// Force 64-bit chunk offsets (normally chosen automatically above 4 GiB).
    pub force_co64: bool,
}

/// What was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mp4Summary {
    pub bytes: u64,
    /// Longest track presentation duration.
    pub duration: Nanos,
    pub samples: usize,
}

const MOVIE_TIMESCALE: u32 = 1000;
const VIDEO_TIMESCALE: u32 = 90_000;
/// Seconds between 1904-01-01 (MP4 epoch) and 1970-01-01.
const MP4_EPOCH_OFFSET: u64 = 2_082_844_800;
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
const LANGUAGE_UND: u16 = 0x55C4;

/// Timing of one track converted to its media timescale.
struct TrackPlan<'a> {
    info: &'a TrackInfo,
    packets: &'a [Packet],
    track_id: u32,
    timescale: u32,
    durations: Vec<u32>,
    cts_offsets: Vec<i32>,
    media_duration: u64,
    /// Presentation time of the first sample relative to the file origin.
    empty_edit: Nanos,
    /// Composition offset of the first sample, in media timescale.
    media_time: i64,
    /// Chunk offsets, filled in once the layout is known.
    offsets: Vec<u64>,
}

/// Round `ns * timescale / 1e9` to the nearest integer, using 128-bit math.
fn ns_to_ts(ns: i64, timescale: u32) -> i64 {
    let num = i128::from(ns).saturating_mul(i128::from(timescale));
    let half = i128::from(NANOS_PER_SEC / 2);
    let den = i128::from(NANOS_PER_SEC);
    let adj = if num >= 0 {
        num.saturating_add(half)
    } else {
        num.saturating_sub(half)
    };
    let q = adj.checked_div(den).unwrap_or(0);
    i64::try_from(q).unwrap_or(if q > 0 { i64::MAX } else { i64::MIN })
}

fn ts_to_ns(ts: i64, timescale: u32) -> Nanos {
    if timescale == 0 {
        return 0;
    }
    let num = i128::from(ts).saturating_mul(i128::from(NANOS_PER_SEC));
    let q = num.checked_div(i128::from(timescale)).unwrap_or(0);
    i64::try_from(q).unwrap_or(i64::MAX)
}

fn plan_track<'a>(index: usize, t: &Mp4Track<'a>) -> Result<TrackPlan<'a>, Mp4Error> {
    t.info
        .validate()
        .map_err(|e| Mp4Error::InvalidTrack(index, e.to_string()))?;
    let first = t.packets.first().ok_or(Mp4Error::EmptyTrack(index))?;
    let timescale = match &t.info.params {
        TrackParams::Video(_) => VIDEO_TIMESCALE,
        TrackParams::Audio(a) => a.sample_rate,
    };
    let n = t.packets.len();
    let mut dts_ts = Vec::with_capacity(n);
    let mut cts_offsets = Vec::with_capacity(n);
    let mut prev_dts = first.dts;
    for p in t.packets {
        if p.dts < prev_dts {
            return Err(Mp4Error::NonMonotonic(index));
        }
        prev_dts = p.dts;
        let rel = p
            .dts
            .checked_sub(first.dts)
            .ok_or(Mp4Error::TooLarge("timestamp"))?;
        dts_ts.push(ns_to_ts(rel, timescale));
        let cts = ns_to_ts(p.pts.saturating_sub(p.dts), timescale);
        cts_offsets.push(i32::try_from(cts).map_err(|_| Mp4Error::TooLarge("composition offset"))?);
    }
    let mut durations = Vec::with_capacity(n);
    for pair in dts_ts.windows(2) {
        let (a, b) = match pair {
            [a, b] => (*a, *b),
            _ => return Err(Mp4Error::Internal("window size")),
        };
        let d = b
            .checked_sub(a)
            .ok_or(Mp4Error::TooLarge("sample duration"))?;
        durations.push(u32::try_from(d).map_err(|_| Mp4Error::TooLarge("sample duration"))?);
    }
    // The last sample keeps its own duration, else the previous one, else a default.
    let last_pkt_duration = t.packets.last().map(|p| p.duration).unwrap_or(0);
    let last = if last_pkt_duration > 0 {
        u32::try_from(ns_to_ts(last_pkt_duration, timescale))
            .map_err(|_| Mp4Error::TooLarge("sample duration"))?
    } else if let Some(prev) = durations.last() {
        *prev
    } else {
        timescale.checked_div(30).unwrap_or(1).max(1)
    };
    durations.push(last);
    let media_duration = durations
        .iter()
        .try_fold(0u64, |acc, d| acc.checked_add(u64::from(*d)))
        .ok_or(Mp4Error::TooLarge("track duration"))?;
    let media_time = i64::from(cts_offsets.first().copied().unwrap_or(0).max(0));
    Ok(TrackPlan {
        info: t.info,
        packets: t.packets,
        track_id: u32::try_from(index.saturating_add(1))
            .map_err(|_| Mp4Error::TooLarge("track count"))?,
        timescale,
        durations,
        cts_offsets,
        media_duration,
        empty_edit: 0,
        media_time,
        offsets: Vec::new(),
    })
}

impl TrackPlan<'_> {
    /// Presentation duration in nanoseconds, excluding the leading empty edit.
    fn presented_ns(&self) -> Nanos {
        let media = i64::try_from(self.media_duration).unwrap_or(i64::MAX);
        ts_to_ns(media.saturating_sub(self.media_time), self.timescale)
    }

    fn total_ns(&self) -> Nanos {
        self.empty_edit.saturating_add(self.presented_ns())
    }

    fn needs_edit_list(&self) -> bool {
        self.empty_edit > 0 || self.media_time != 0
    }
}

/// Write an MP4 to `out`. Tracks keep their order; give video first.
pub fn write_mp4<W: Write>(
    out: &mut W,
    tracks: &[Mp4Track<'_>],
    opts: &Mp4Options,
) -> Result<Mp4Summary, Mp4Error> {
    if tracks.is_empty() {
        return Err(Mp4Error::NoTracks);
    }
    let mut plans = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| plan_track(i, t))
        .collect::<Result<Vec<_>, _>>()?;

    // Align tracks on the earliest presentation time.
    let origin = tracks
        .iter()
        .filter_map(|t| t.packets.first().map(|p| p.pts))
        .min()
        .ok_or(Mp4Error::NoTracks)?;
    for (plan, t) in plans.iter_mut().zip(tracks) {
        let first_pts = t.packets.first().map(|p| p.pts).unwrap_or(origin);
        plan.empty_edit = first_pts.saturating_sub(origin).max(0);
    }

    // Interleave samples by decode time (stable on track order).
    let mut order: Vec<(Nanos, usize, usize)> = Vec::new();
    for (ti, t) in tracks.iter().enumerate() {
        for (si, p) in t.packets.iter().enumerate() {
            order.push((p.dts, ti, si));
        }
    }
    order.sort_by_key(|&(dts, ti, si)| (dts, ti, si));

    let data_size = tracks
        .iter()
        .flat_map(|t| t.packets.iter())
        .try_fold(0u64, |acc, p| {
            acc.checked_add(u64::try_from(p.data.len()).ok()?)
        })
        .ok_or(Mp4Error::TooLarge("media data"))?;
    let ftyp = build_ftyp(tracks);
    let ftyp_len = u64::try_from(ftyp.len()).map_err(|_| Mp4Error::Internal("ftyp size"))?;
    let mdat_header_len: u64 = if data_size > u64::from(u32::MAX).saturating_sub(8) {
        16
    } else {
        8
    };

    // First pass with 32-bit offsets (unless forced) to learn the moov size.
    let mut use_co64 = opts.force_co64;
    let moov = loop {
        let probe = build_moov(&mut plans, &order, 0, use_co64, opts)?;
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
        let real = build_moov(&mut plans, &order, data_start, use_co64, opts)?;
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
        let p = tracks
            .get(ti)
            .and_then(|t| t.packets.get(si))
            .ok_or(Mp4Error::Internal("sample index"))?;
        out.write_all(&p.data)?;
    }
    out.flush()?;

    let bytes = ftyp_len
        .saturating_add(u64::try_from(moov.len()).unwrap_or(u64::MAX))
        .saturating_add(mdat_header_len)
        .saturating_add(data_size);
    Ok(Mp4Summary {
        bytes,
        duration: plans.iter().map(TrackPlan::total_ns).max().unwrap_or(0),
        samples: order.len(),
    })
}

/// Write an MP4 atomically: to `<path>.part` in the same folder, fsync, then rename.
/// Refuses to overwrite an existing file. The temp file is removed on failure.
pub fn save_mp4_atomic(
    path: &Path,
    tracks: &[Mp4Track<'_>],
    opts: &Mp4Options,
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

fn build_ftyp(tracks: &[Mp4Track<'_>]) -> Vec<u8> {
    let mut b = BoxBuf::new();
    let at = b.start(b"ftyp");
    b.bytes(b"isom");
    b.u32(0x200);
    b.bytes(b"isom");
    b.bytes(b"iso2");
    b.bytes(b"mp41");
    for t in tracks {
        match t.info.codec {
            Codec::H264 => b.bytes(b"avc1"),
            Codec::Av1 => b.bytes(b"av01"),
            _ => {}
        }
    }
    // A 32-byte ftyp can't overflow; fall back to an empty vec if it somehow did.
    if b.finish(at).is_err() {
        return Vec::new();
    }
    b.buf
}

fn mp4_time(opts: &Mp4Options) -> u64 {
    opts.creation_time_unix.saturating_add(MP4_EPOCH_OFFSET)
}

fn build_moov(
    plans: &mut [TrackPlan<'_>],
    order: &[(Nanos, usize, usize)],
    data_start: u64,
    co64: bool,
    opts: &Mp4Options,
) -> Result<Vec<u8>, Mp4Error> {
    // Chunk offsets in interleave order.
    for p in plans.iter_mut() {
        p.offsets = vec![0; p.packets.len()];
    }
    let mut pos = data_start;
    for &(_, ti, si) in order {
        let plan = plans.get_mut(ti).ok_or(Mp4Error::Internal("track index"))?;
        let size = plan
            .packets
            .get(si)
            .map(|p| p.data.len())
            .ok_or(Mp4Error::Internal("sample index"))?;
        let slot = plan
            .offsets
            .get_mut(si)
            .ok_or(Mp4Error::Internal("offset index"))?;
        *slot = pos;
        pos = pos
            .checked_add(u64::try_from(size).map_err(|_| Mp4Error::TooLarge("sample"))?)
            .ok_or(Mp4Error::TooLarge("file"))?;
    }

    let now = mp4_time(opts);
    let movie_duration = plans
        .iter()
        .map(|p| ns_to_ts(p.total_ns(), MOVIE_TIMESCALE))
        .max()
        .unwrap_or(0);
    let movie_duration = u64::try_from(movie_duration).unwrap_or(0);

    let mut b = BoxBuf::new();
    let moov = b.start(b"moov");
    write_mvhd(&mut b, now, movie_duration, plans.len())?;
    for plan in plans.iter() {
        write_trak(&mut b, plan, now, co64)?;
    }
    b.finish(moov)?;
    Ok(b.buf)
}

fn needs_v1(values: &[u64]) -> bool {
    values.iter().any(|v| *v > u64::from(u32::MAX))
}

fn write_time_fields(b: &mut BoxBuf, v1: bool, created: u64) {
    if v1 {
        b.u64(created);
        b.u64(created);
    } else {
        b.u32(u32::try_from(created).unwrap_or(0));
        b.u32(u32::try_from(created).unwrap_or(0));
    }
}

fn write_duration(b: &mut BoxBuf, v1: bool, d: u64) {
    if v1 {
        b.u64(d);
    } else {
        b.u32(u32::try_from(d).unwrap_or(u32::MAX));
    }
}

fn write_mvhd(b: &mut BoxBuf, now: u64, duration: u64, tracks: usize) -> Result<(), Mp4Error> {
    let v1 = needs_v1(&[now, duration]);
    let at = b.start_full(b"mvhd", u8::from(v1), 0);
    write_time_fields(b, v1, now);
    b.u32(MOVIE_TIMESCALE);
    write_duration(b, v1, duration);
    b.u32(0x0001_0000); // rate 1.0
    b.u16(0x0100); // volume 1.0
    b.zeros(10);
    for m in MATRIX {
        b.u32(m);
    }
    b.zeros(24); // pre_defined
    let next = u32::try_from(tracks.saturating_add(1)).map_err(|_| Mp4Error::TooLarge("tracks"))?;
    b.u32(next);
    b.finish(at)
}

fn write_trak(b: &mut BoxBuf, p: &TrackPlan<'_>, now: u64, co64: bool) -> Result<(), Mp4Error> {
    let trak = b.start(b"trak");
    let tk_duration = u64::try_from(ns_to_ts(p.total_ns(), MOVIE_TIMESCALE)).unwrap_or(0);
    // tkhd
    let v1 = needs_v1(&[now, tk_duration]);
    let at = b.start_full(b"tkhd", u8::from(v1), 0x3); // enabled | in movie
    write_time_fields(b, v1, now);
    b.u32(p.track_id);
    b.u32(0);
    write_duration(b, v1, tk_duration);
    b.zeros(8);
    b.i16(0); // layer
    b.i16(0); // alternate group
    let (volume, width, height) = match &p.info.params {
        TrackParams::Video(v) => (0u16, v.width, v.height),
        TrackParams::Audio(_) => (0x0100, 0, 0),
    };
    b.u16(volume);
    b.u16(0);
    for m in MATRIX {
        b.u32(m);
    }
    b.u32(fixed_16_16(width)?);
    b.u32(fixed_16_16(height)?);
    b.finish(at)?;

    if p.needs_edit_list() {
        write_edts(b, p)?;
    }

    let mdia = b.start(b"mdia");
    // mdhd
    let v1 = needs_v1(&[now, p.media_duration]);
    let at = b.start_full(b"mdhd", u8::from(v1), 0);
    write_time_fields(b, v1, now);
    b.u32(p.timescale);
    write_duration(b, v1, p.media_duration);
    b.u16(LANGUAGE_UND);
    b.u16(0);
    b.finish(at)?;
    // hdlr
    let (handler, name): (&[u8; 4], &[u8]) = match p.info.params {
        TrackParams::Video(_) => (b"vide", b"VideoHandler\0"),
        TrackParams::Audio(_) => (b"soun", b"SoundHandler\0"),
    };
    let at = b.start_full(b"hdlr", 0, 0);
    b.u32(0);
    b.bytes(handler);
    b.zeros(12);
    b.bytes(name);
    b.finish(at)?;

    let minf = b.start(b"minf");
    match p.info.params {
        TrackParams::Video(_) => {
            let at = b.start_full(b"vmhd", 0, 1);
            b.zeros(8);
            b.finish(at)?;
        }
        TrackParams::Audio(_) => {
            let at = b.start_full(b"smhd", 0, 0);
            b.zeros(4);
            b.finish(at)?;
        }
    }
    let dinf = b.start(b"dinf");
    let dref = b.start_full(b"dref", 0, 0);
    b.u32(1);
    let url = b.start_full(b"url ", 0, 1); // self-contained
    b.finish(url)?;
    b.finish(dref)?;
    b.finish(dinf)?;
    write_stbl(b, p, co64)?;
    b.finish(minf)?;
    b.finish(mdia)?;
    b.finish(trak)
}

fn fixed_16_16(v: u32) -> Result<u32, Mp4Error> {
    if v > 0xFFFF {
        return Err(Mp4Error::TooLarge("dimension"));
    }
    Ok(v << 16)
}

fn write_edts(b: &mut BoxBuf, p: &TrackPlan<'_>) -> Result<(), Mp4Error> {
    let empty = u64::try_from(ns_to_ts(p.empty_edit, MOVIE_TIMESCALE)).unwrap_or(0);
    let presented = u64::try_from(ns_to_ts(p.presented_ns(), MOVIE_TIMESCALE)).unwrap_or(0);
    let v1 = needs_v1(&[empty, presented]) || i32::try_from(p.media_time).is_err();
    let mut entries: Vec<(u64, i64)> = Vec::new();
    if empty > 0 {
        entries.push((empty, -1));
    }
    entries.push((presented, p.media_time));
    let edts = b.start(b"edts");
    let at = b.start_full(b"elst", u8::from(v1), 0);
    b.u32(u32::try_from(entries.len()).map_err(|_| Mp4Error::Internal("elst"))?);
    for (dur, media_time) in entries {
        if v1 {
            b.u64(dur);
            b.i64(media_time);
        } else {
            b.u32(u32::try_from(dur).map_err(|_| Mp4Error::TooLarge("edit"))?);
            b.i32(i32::try_from(media_time).map_err(|_| Mp4Error::TooLarge("edit"))?);
        }
        b.i16(1); // media rate
        b.i16(0);
    }
    b.finish(at)?;
    b.finish(edts)
}

/// Run-length encode equal neighbours.
fn runs<T: PartialEq + Copy>(values: &[T]) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for &v in values {
        match out.last_mut() {
            Some((count, last)) if *last == v && *count < u32::MAX => {
                *count = count.saturating_add(1)
            }
            _ => out.push((1, v)),
        }
    }
    out
}

fn count_u32(n: usize) -> Result<u32, Mp4Error> {
    u32::try_from(n).map_err(|_| Mp4Error::TooLarge("sample count"))
}

fn write_stbl(b: &mut BoxBuf, p: &TrackPlan<'_>, co64: bool) -> Result<(), Mp4Error> {
    let stbl = b.start(b"stbl");
    // stsd
    let at = b.start_full(b"stsd", 0, 0);
    b.u32(1);
    write_sample_entry(b, p)?;
    b.finish(at)?;
    // stts
    let stts = runs(&p.durations);
    let at = b.start_full(b"stts", 0, 0);
    b.u32(count_u32(stts.len())?);
    for (count, delta) in stts {
        b.u32(count);
        b.u32(delta);
    }
    b.finish(at)?;
    // ctts (version 1: signed offsets), only if some sample needs it
    if p.cts_offsets.iter().any(|o| *o != 0) {
        let ctts = runs(&p.cts_offsets);
        let at = b.start_full(b"ctts", 1, 0);
        b.u32(count_u32(ctts.len())?);
        for (count, off) in ctts {
            b.u32(count);
            b.i32(off);
        }
        b.finish(at)?;
    }
    // stss, only if not every sample is a sync sample
    if p.packets.iter().any(|pk| !pk.keyframe) {
        let keys: Vec<u32> = p
            .packets
            .iter()
            .enumerate()
            .filter(|(_, pk)| pk.keyframe)
            .map(|(i, _)| count_u32(i.saturating_add(1)))
            .collect::<Result<_, _>>()?;
        let at = b.start_full(b"stss", 0, 0);
        b.u32(count_u32(keys.len())?);
        for k in keys {
            b.u32(k);
        }
        b.finish(at)?;
    }
    // stsc: every chunk holds one sample
    let at = b.start_full(b"stsc", 0, 0);
    b.u32(1);
    b.u32(1);
    b.u32(1);
    b.u32(1);
    b.finish(at)?;
    // stsz
    let at = b.start_full(b"stsz", 0, 0);
    b.u32(0);
    b.u32(count_u32(p.packets.len())?);
    for pk in p.packets {
        b.u32(u32::try_from(pk.data.len()).map_err(|_| Mp4Error::TooLarge("sample size"))?);
    }
    b.finish(at)?;
    // stco / co64
    if co64 {
        let at = b.start_full(b"co64", 0, 0);
        b.u32(count_u32(p.offsets.len())?);
        for o in &p.offsets {
            b.u64(*o);
        }
        b.finish(at)?;
    } else {
        let at = b.start_full(b"stco", 0, 0);
        b.u32(count_u32(p.offsets.len())?);
        for o in &p.offsets {
            b.u32(u32::try_from(*o).map_err(|_| Mp4Error::TooLarge("chunk offset"))?);
        }
        b.finish(at)?;
    }
    b.finish(stbl)
}

fn write_sample_entry(b: &mut BoxBuf, p: &TrackPlan<'_>) -> Result<(), Mp4Error> {
    match (&p.info.params, p.info.codec) {
        (TrackParams::Video(v), codec) => {
            let (entry, config): (&[u8; 4], &[u8; 4]) = match codec {
                Codec::H264 => (b"avc1", b"avcC"),
                Codec::H265 => (b"hvc1", b"hvcC"),
                Codec::Av1 => (b"av01", b"av1C"),
                Codec::Aac | Codec::Opus => {
                    return Err(Mp4Error::Internal("audio codec on video track"));
                }
            };
            let at = b.start(entry);
            b.zeros(6);
            b.u16(1); // data_reference_index
            b.zeros(16);
            b.u16(u16::try_from(v.width).map_err(|_| Mp4Error::TooLarge("width"))?);
            b.u16(u16::try_from(v.height).map_err(|_| Mp4Error::TooLarge("height"))?);
            b.u32(0x0048_0000); // 72 dpi
            b.u32(0x0048_0000);
            b.u32(0);
            b.u16(1); // frame_count
            b.zeros(32); // compressorname
            b.u16(0x0018);
            b.i16(-1);
            let c = b.start(config);
            b.bytes(&p.info.codec_data);
            b.finish(c)?;
            if let Some(col) = v.colorimetry {
                let c = b.start(b"colr");
                b.bytes(b"nclx");
                b.u16(col.primaries);
                b.u16(col.transfer);
                b.u16(col.matrix);
                b.u8(if col.full_range { 0x80 } else { 0 });
                b.finish(c)?;
            }
            let c = b.start(b"pasp");
            b.u32(1);
            b.u32(1);
            b.finish(c)?;
            b.finish(at)
        }
        (TrackParams::Audio(a), codec) => {
            let entry: &[u8; 4] = match codec {
                Codec::Aac => b"mp4a",
                Codec::Opus => b"Opus",
                _ => return Err(Mp4Error::Internal("video codec on audio track")),
            };
            let rate = if codec == Codec::Opus {
                48_000
            } else {
                a.sample_rate
            };
            let rate16 = u16::try_from(rate)
                .map_err(|_| Mp4Error::Unsupported("sample rate above 65535 Hz"))?;
            let at = b.start(entry);
            b.zeros(6);
            b.u16(1);
            b.zeros(8);
            b.u16(a.channels);
            b.u16(16);
            b.u16(0);
            b.u16(0);
            b.u32(u32::from(rate16) << 16);
            match codec {
                Codec::Aac => write_esds(b, p)?,
                _ => write_dops(b, &p.info.codec_data)?,
            }
            b.finish(at)
        }
    }
}

fn write_esds(b: &mut BoxBuf, p: &TrackPlan<'_>) -> Result<(), Mp4Error> {
    let bitrate = p.info.bitrate.unwrap_or(0);
    let mut dsi = BoxBuf::new();
    dsi.descriptor(0x05, &p.info.codec_data)?;
    let mut dcd = BoxBuf::new();
    dcd.u8(0x40); // MPEG-4 Audio
    dcd.u8(0x15); // audio stream, upstream = 0, reserved = 1
    dcd.u24(0); // bufferSizeDB
    dcd.u32(bitrate);
    dcd.u32(bitrate);
    dcd.bytes(&dsi.buf);
    let mut es = BoxBuf::new();
    es.u16(u16::try_from(p.track_id).unwrap_or(1));
    es.u8(0);
    es.descriptor(0x04, &dcd.buf)?;
    es.descriptor(0x06, &[0x02])?;
    let at = b.start_full(b"esds", 0, 0);
    b.descriptor(0x03, &es.buf)?;
    b.finish(at)
}

/// Convert an `OpusHead` packet (little-endian, RFC 7845) into a `dOps` box (big-endian).
fn write_dops(b: &mut BoxBuf, head: &[u8]) -> Result<(), Mp4Error> {
    let bad = Mp4Error::Unsupported("Opus codec data is not an OpusHead packet");
    if head.get(0..8) != Some(b"OpusHead".as_slice()) {
        return Err(bad);
    }
    let channels = *head.get(9).ok_or(Mp4Error::Unsupported("short OpusHead"))?;
    let le16 = |i: usize| -> Option<u16> {
        let s = head.get(i..i.checked_add(2)?)?;
        Some(u16::from_le_bytes(s.try_into().ok()?))
    };
    let le32 = |i: usize| -> Option<u32> {
        let s = head.get(i..i.checked_add(4)?)?;
        Some(u32::from_le_bytes(s.try_into().ok()?))
    };
    let short = || Mp4Error::Unsupported("short OpusHead");
    let pre_skip = le16(10).ok_or_else(short)?;
    let rate = le32(12).ok_or_else(short)?;
    let gain = le16(16).ok_or_else(short)?;
    let family = *head.get(18).ok_or_else(short)?;
    let at = b.start(b"dOps");
    b.u8(0);
    b.u8(channels);
    b.u16(pre_skip);
    b.u32(rate);
    b.u16(gain);
    b.u8(family);
    if family != 0 {
        let table_len = usize::from(channels).checked_add(2).ok_or_else(short)?;
        let end = 19usize.checked_add(table_len).ok_or_else(short)?;
        b.bytes(head.get(19..end).ok_or_else(short)?);
    }
    b.finish(at)
}

#[cfg(test)]
mod tests;
