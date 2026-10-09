//! `ftyp` and `moov` building, shared by the faststart clip writer, the fragmented
//! writer's init segment and the `moov` written when a fragmented recording is finalised.

use klip_media::{Codec, NANOS_PER_SEC, Nanos, Packet, TrackInfo, TrackParams};

use crate::boxes::BoxBuf;
use crate::{MAX_TRACK_NAME_BYTES, Mp4Error, Mp4Track};

pub(crate) const MOVIE_TIMESCALE: u32 = 1000;
pub(crate) const VIDEO_TIMESCALE: u32 = 90_000;
/// Seconds between 1904-01-01 (MP4 epoch) and 1970-01-01.
pub(crate) const MP4_EPOCH_OFFSET: u64 = 2_082_844_800;
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
const LANGUAGE_UND: u16 = 0x55C4;
/// Most markers a `chpl` box can list (its count is one byte).
pub(crate) const MAX_CHAPTERS: usize = 255;

/// Timing and sample tables of one track, in its media timescale.
pub(crate) struct TrackPlan<'a> {
    pub(crate) info: &'a TrackInfo,
    pub(crate) name: Option<&'a str>,
    pub(crate) enabled: bool,
    /// `tkhd` alternate group (0 = none).
    pub(crate) alternate_group: i16,
    pub(crate) track_id: u32,
    pub(crate) timescale: u32,
    pub(crate) durations: Vec<u32>,
    pub(crate) cts_offsets: Vec<i32>,
    pub(crate) sizes: Vec<u32>,
    pub(crate) sync: Vec<bool>,
    pub(crate) media_duration: u64,
    /// Presentation time of the first sample relative to the file origin.
    pub(crate) empty_edit: Nanos,
    /// Composition offset of the first sample, in media timescale.
    pub(crate) media_time: i64,
    /// (file offset, sample count) of each chunk, in sample order.
    pub(crate) chunks: Vec<(u64, u32)>,
}

/// Round `ns * timescale / 1e9` to the nearest integer, using 128-bit math.
pub(crate) fn ns_to_ts(ns: i64, timescale: u32) -> i64 {
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

pub(crate) fn ts_to_ns(ts: i64, timescale: u32) -> Nanos {
    if timescale == 0 {
        return 0;
    }
    let num = i128::from(ts).saturating_mul(i128::from(NANOS_PER_SEC));
    let q = num.checked_div(i128::from(timescale)).unwrap_or(0);
    i64::try_from(q).unwrap_or(i64::MAX)
}

/// Media timescale of a track: 90 kHz for video, the sample rate for audio.
pub(crate) fn timescale_of(info: &TrackInfo) -> u32 {
    match &info.params {
        TrackParams::Video(_) => VIDEO_TIMESCALE,
        TrackParams::Audio(a) => a.sample_rate,
    }
}

pub(crate) fn track_id(index: usize) -> Result<u32, Mp4Error> {
    u32::try_from(index.saturating_add(1)).map_err(|_| Mp4Error::TooLarge("track count"))
}

pub(crate) fn sample_size(p: &Packet) -> Result<u32, Mp4Error> {
    u32::try_from(p.data.len()).map_err(|_| Mp4Error::TooLarge("sample size"))
}

impl<'a> TrackPlan<'a> {
    /// A plan without samples (init segments).
    pub(crate) fn empty(
        index: usize,
        info: &'a TrackInfo,
        name: Option<&'a str>,
        enabled: bool,
    ) -> Result<TrackPlan<'a>, Mp4Error> {
        info.validate()
            .map_err(|e| Mp4Error::InvalidTrack(index, e.to_string()))?;
        Ok(TrackPlan {
            info,
            name,
            enabled,
            alternate_group: 0,
            track_id: track_id(index)?,
            timescale: timescale_of(info),
            durations: Vec::new(),
            cts_offsets: Vec::new(),
            sizes: Vec::new(),
            sync: Vec::new(),
            media_duration: 0,
            empty_edit: 0,
            media_time: 0,
            chunks: Vec::new(),
        })
    }

    /// Plan for the packets of a clip track. Chunks are filled in by the layout.
    pub(crate) fn from_packets(index: usize, t: &Mp4Track<'a>) -> Result<TrackPlan<'a>, Mp4Error> {
        let mut plan = TrackPlan::empty(index, t.info, t.name, t.enabled)?;
        let first = t.packets.first().ok_or(Mp4Error::EmptyTrack(index))?;
        let timescale = plan.timescale;
        let n = t.packets.len();
        let mut dts_ts = Vec::with_capacity(n);
        plan.cts_offsets.reserve(n);
        plan.sizes.reserve(n);
        plan.sync.reserve(n);
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
            plan.cts_offsets
                .push(i32::try_from(cts).map_err(|_| Mp4Error::TooLarge("composition offset"))?);
            plan.sizes.push(sample_size(p)?);
            plan.sync.push(p.keyframe);
        }
        plan.durations.reserve(n);
        for pair in dts_ts.windows(2) {
            let (a, b) = match pair {
                [a, b] => (*a, *b),
                _ => return Err(Mp4Error::Internal("window size")),
            };
            let d = b
                .checked_sub(a)
                .ok_or(Mp4Error::TooLarge("sample duration"))?;
            plan.durations
                .push(u32::try_from(d).map_err(|_| Mp4Error::TooLarge("sample duration"))?);
        }
        // The last sample keeps its own duration, else the previous one, else a default.
        let last_pkt_duration = t.packets.last().map(|p| p.duration).unwrap_or(0);
        let last = if last_pkt_duration > 0 {
            u32::try_from(ns_to_ts(last_pkt_duration, timescale))
                .map_err(|_| Mp4Error::TooLarge("sample duration"))?
        } else if let Some(prev) = plan.durations.last() {
            *prev
        } else {
            timescale.checked_div(30).unwrap_or(1).max(1)
        };
        plan.durations.push(last);
        plan.finish_timing()?;
        Ok(plan)
    }

    /// Media duration and first composition offset from the sample tables.
    pub(crate) fn finish_timing(&mut self) -> Result<(), Mp4Error> {
        self.media_duration = self
            .durations
            .iter()
            .try_fold(0u64, |acc, d| acc.checked_add(u64::from(*d)))
            .ok_or(Mp4Error::TooLarge("track duration"))?;
        self.media_time = i64::from(self.cts_offsets.first().copied().unwrap_or(0).max(0));
        Ok(())
    }

    /// Presentation duration in nanoseconds, excluding the leading empty edit.
    pub(crate) fn presented_ns(&self) -> Nanos {
        let media = i64::try_from(self.media_duration).unwrap_or(i64::MAX);
        ts_to_ns(media.saturating_sub(self.media_time), self.timescale)
    }

    pub(crate) fn total_ns(&self) -> Nanos {
        self.empty_edit.saturating_add(self.presented_ns())
    }

    fn needs_edit_list(&self) -> bool {
        self.empty_edit > 0 || self.media_time != 0
    }

    pub(crate) fn samples(&self) -> usize {
        self.sizes.len()
    }
}

/// Several audio tracks are alternatives to each other (mix, game, mic): players pick the
/// enabled one rather than playing all of them at once.
pub(crate) fn group_audio_tracks(plans: &mut [TrackPlan<'_>]) {
    let audio = plans
        .iter()
        .filter(|p| matches!(p.info.params, TrackParams::Audio(_)))
        .count();
    if audio > 1 {
        for p in plans
            .iter_mut()
            .filter(|p| matches!(p.info.params, TrackParams::Audio(_)))
        {
            p.alternate_group = 1;
        }
    }
}

/// `ftyp` for the given tracks. Fragmented files also list `iso5` (movie fragments with
/// `default-base-is-moof`).
pub(crate) fn build_ftyp(infos: &[&TrackInfo], fragmented: bool) -> Vec<u8> {
    let mut b = BoxBuf::new();
    let at = b.start(b"ftyp");
    b.bytes(b"isom");
    b.u32(0x200);
    b.bytes(b"isom");
    b.bytes(b"iso2");
    if fragmented {
        b.bytes(b"iso5");
    }
    b.bytes(b"mp41");
    for info in infos {
        match info.codec {
            Codec::H264 => b.bytes(b"avc1"),
            Codec::Av1 => b.bytes(b"av01"),
            _ => {}
        }
    }
    // A small ftyp can't overflow; fall back to an empty vec if it somehow did.
    if b.finish(at).is_err() {
        return Vec::new();
    }
    b.buf
}

/// A chapter marker for `udta/chpl`: presentation time (100 ns units) and title.
pub(crate) struct Chapter<'a> {
    pub(crate) at_100ns: u64,
    pub(crate) title: &'a str,
}

/// What kind of `moov` to build.
pub(crate) enum MoovKind<'a> {
    /// Full sample tables, edit lists and chapters.
    Regular { chapters: &'a [Chapter<'a>] },
    /// Init segment of a fragmented file: empty sample tables and `mvex`.
    FragmentInit,
}

pub(crate) fn build_moov(
    plans: &[TrackPlan<'_>],
    kind: &MoovKind<'_>,
    co64: bool,
    creation_time_unix: u64,
) -> Result<Vec<u8>, Mp4Error> {
    let now = creation_time_unix.saturating_add(MP4_EPOCH_OFFSET);
    let regular = matches!(kind, MoovKind::Regular { .. });
    let movie_duration = if regular {
        let d = plans
            .iter()
            .map(|p| ns_to_ts(p.total_ns(), MOVIE_TIMESCALE))
            .max()
            .unwrap_or(0);
        u64::try_from(d).unwrap_or(0)
    } else {
        0
    };

    let mut b = BoxBuf::new();
    let moov = b.start(b"moov");
    write_mvhd(&mut b, now, movie_duration, plans.len())?;
    for plan in plans {
        write_trak(&mut b, plan, now, co64, regular)?;
    }
    match kind {
        MoovKind::FragmentInit => {
            let mvex = b.start(b"mvex");
            for plan in plans {
                let at = b.start_full(b"trex", 0, 0);
                b.u32(plan.track_id);
                b.u32(1); // default_sample_description_index
                b.u32(0); // default_sample_duration
                b.u32(0); // default_sample_size
                b.u32(0); // default_sample_flags
                b.finish(at)?;
            }
            b.finish(mvex)?;
        }
        MoovKind::Regular { chapters } => {
            if !chapters.is_empty() {
                write_chapters(&mut b, chapters)?;
            }
        }
    }
    b.finish(moov)?;
    Ok(b.buf)
}

/// Nero-style chapter list (`udta/chpl`, version 1): read by ffmpeg/ffprobe, VLC, mpv.
fn write_chapters(b: &mut BoxBuf, chapters: &[Chapter<'_>]) -> Result<(), Mp4Error> {
    let chapters = chapters.get(..MAX_CHAPTERS).unwrap_or(chapters);
    let udta = b.start(b"udta");
    let at = b.start_full(b"chpl", 1, 0);
    b.u32(0); // reserved
    b.u8(u8::try_from(chapters.len()).map_err(|_| Mp4Error::Internal("chapter count"))?);
    for c in chapters {
        b.u64(c.at_100ns);
        let title = cut_utf8(c.title, 255);
        b.u8(u8::try_from(title.len()).map_err(|_| Mp4Error::Internal("chapter title"))?);
        b.bytes(title.as_bytes());
    }
    b.finish(at)?;
    b.finish(udta)
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

fn write_trak(
    b: &mut BoxBuf,
    p: &TrackPlan<'_>,
    now: u64,
    co64: bool,
    regular: bool,
) -> Result<(), Mp4Error> {
    let trak = b.start(b"trak");
    let tk_duration = if regular {
        u64::try_from(ns_to_ts(p.total_ns(), MOVIE_TIMESCALE)).unwrap_or(0)
    } else {
        0
    };
    // tkhd
    let v1 = needs_v1(&[now, tk_duration]);
    // in movie, plus enabled.
    let flags = if p.enabled { 0x3 } else { 0x2 };
    let at = b.start_full(b"tkhd", u8::from(v1), flags);
    write_time_fields(b, v1, now);
    b.u32(p.track_id);
    b.u32(0);
    write_duration(b, v1, tk_duration);
    b.zeros(8);
    b.i16(0); // layer
    b.i16(p.alternate_group);
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

    if regular && p.needs_edit_list() {
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
    let (handler, default_name) = match p.info.params {
        TrackParams::Video(_) => (b"vide", "VideoHandler"),
        TrackParams::Audio(_) => (b"soun", "SoundHandler"),
    };
    let at = b.start_full(b"hdlr", 0, 0);
    b.u32(0);
    b.bytes(handler);
    b.zeros(12);
    b.bytes(track_name(p.name.unwrap_or(default_name)).as_bytes());
    b.u8(0);
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

/// `s` cut to at most `max` bytes at a character boundary.
fn cut_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or("")
}

/// `name` without NULs, cut to [`MAX_TRACK_NAME_BYTES`] at a character boundary.
pub(crate) fn track_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().filter(|c| *c != '\0') {
        if out.len().saturating_add(c.len_utf8()) > MAX_TRACK_NAME_BYTES {
            break;
        }
        out.push(c);
    }
    out
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
fn runs<T: PartialEq + Copy>(values: impl IntoIterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
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
    let stts = runs(p.durations.iter().copied());
    let at = b.start_full(b"stts", 0, 0);
    b.u32(count_u32(stts.len())?);
    for (count, delta) in stts {
        b.u32(count);
        b.u32(delta);
    }
    b.finish(at)?;
    // ctts (version 1: signed offsets), only if some sample needs it
    if p.cts_offsets.iter().any(|o| *o != 0) {
        let ctts = runs(p.cts_offsets.iter().copied());
        let at = b.start_full(b"ctts", 1, 0);
        b.u32(count_u32(ctts.len())?);
        for (count, off) in ctts {
            b.u32(count);
            b.i32(off);
        }
        b.finish(at)?;
    }
    // stss, only if not every sample is a sync sample
    if p.sync.iter().any(|s| !s) {
        let keys: Vec<u32> = p
            .sync
            .iter()
            .enumerate()
            .filter(|(_, s)| **s)
            .map(|(i, _)| count_u32(i.saturating_add(1)))
            .collect::<Result<_, _>>()?;
        let at = b.start_full(b"stss", 0, 0);
        b.u32(count_u32(keys.len())?);
        for k in keys {
            b.u32(k);
        }
        b.finish(at)?;
    }
    // stsc: one entry per run of chunks with the same sample count
    let at = b.start_full(b"stsc", 0, 0);
    let per_chunk = runs(p.chunks.iter().map(|c| c.1));
    b.u32(count_u32(per_chunk.len())?);
    let mut first_chunk: u32 = 1;
    for (count, samples) in per_chunk {
        b.u32(first_chunk);
        b.u32(samples);
        b.u32(1);
        first_chunk = first_chunk
            .checked_add(count)
            .ok_or(Mp4Error::TooLarge("chunk count"))?;
    }
    b.finish(at)?;
    // stsz
    let at = b.start_full(b"stsz", 0, 0);
    b.u32(0);
    b.u32(count_u32(p.sizes.len())?);
    for s in &p.sizes {
        b.u32(*s);
    }
    b.finish(at)?;
    // stco / co64
    if co64 {
        let at = b.start_full(b"co64", 0, 0);
        b.u32(count_u32(p.chunks.len())?);
        for (o, _) in &p.chunks {
            b.u64(*o);
        }
        b.finish(at)?;
    } else {
        let at = b.start_full(b"stco", 0, 0);
        b.u32(count_u32(p.chunks.len())?);
        for (o, _) in &p.chunks {
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
