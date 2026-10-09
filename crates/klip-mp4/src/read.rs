//! Reading fragmented recordings back: the init `moov` (track descriptions) and every
//! complete fragment (sample tables and data positions). Used to finalise a recording and
//! to recover one whose recorder was killed.
//!
//! The input is treated as hostile: every size is checked against the file and the box it
//! sits in, allocations are capped, and scanning stops (without error) at the first
//! fragment that is incomplete or doesn't make sense, keeping everything before it.
//! Fragments and the init `moov` that [`crate::finalize`] already turned into `free`
//! boxes are still recognised, so finalising can be repeated after an interruption.

use std::io::{Read, Seek, SeekFrom};

use bytes::Bytes;
use klip_media::{
    AudioParams, Codec, Colorimetry, MAX_CODEC_DATA_BYTES, TrackInfo, TrackParams, VideoParams,
};

use crate::Mp4Error;
use crate::fragmented::{
    TFHD_DEFAULT_BASE_IS_MOOF, TRUN_CTS, TRUN_DATA_OFFSET, TRUN_DURATION, TRUN_FIRST_SAMPLE_FLAGS,
    TRUN_FLAGS, TRUN_SIZE,
};
use crate::moov::MP4_EPOCH_OFFSET;

/// Largest init `moov` read.
const MAX_INIT_BYTES: u64 = 16 * 1024 * 1024;
/// Largest `moof` read.
const MAX_MOOF_BYTES: u64 = 16 * 1024 * 1024;
/// Most tracks accepted.
const MAX_TRACKS: usize = 64;
/// Most samples accepted in one `trun` (ours hold a few hundred).
const MAX_RUN_SAMPLES: usize = 1_000_000;
/// A child box: type and payload.
pub(crate) type Child<'a> = ([u8; 4], &'a [u8]);

/// `sample_is_non_sync_sample` in sample flags.
const NON_SYNC: u32 = 0x0001_0000;

/// One sample found in a fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScannedSample {
    /// Decode time in the track timescale.
    pub decode_time: u64,
    pub duration: u32,
    pub cts_offset: i32,
    pub size: u32,
    pub sync: bool,
}

/// A track of a fragmented recording.
#[derive(Clone, Debug, PartialEq)]
pub struct ScannedTrack {
    pub info: TrackInfo,
    /// `hdlr` name.
    pub name: Option<String>,
    pub enabled: bool,
    pub track_id: u32,
    pub timescale: u32,
    pub samples: Vec<ScannedSample>,
    /// (file offset, sample count) of each run of contiguous samples, in sample order.
    pub runs: Vec<(u64, u32)>,
}

/// What [`scan_fragmented`] found.
#[derive(Clone, Debug, PartialEq)]
pub struct FragmentedScan {
    pub tracks: Vec<ScannedTrack>,
    pub creation_time_unix: u64,
    /// Offset of the init `moov`, if it still has that type.
    pub init_moov: Option<u64>,
    /// The init `moov` was already turned into `free`: the file is finalised.
    pub finalized: bool,
    /// Offsets of fragments whose `moof` still has that type.
    pub moofs: Vec<u64>,
    pub fragments: usize,
    /// End of the last complete fragment (or of the init segment).
    pub data_end: u64,
    pub file_len: u64,
    /// Why scanning stopped before the end of the file, if it did.
    pub stopped: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Trex {
    track_id: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

/// Scan a fragmented MP4 written by [`crate::FragmentedWriter`].
pub fn scan_fragmented<R: Read + Seek>(r: &mut R) -> Result<FragmentedScan, Mp4Error> {
    let file_len = r.seek(SeekFrom::End(0))?;
    let ftyp = read_header(r, 0, file_len)?.ok_or(Mp4Error::NotFragmented("no ftyp"))?;
    if &ftyp.kind != b"ftyp" {
        return Err(Mp4Error::NotFragmented("no ftyp"));
    }
    let init =
        read_header(r, ftyp.end(), file_len)?.ok_or(Mp4Error::NotFragmented("no init moov"))?;
    let finalized = match &init.kind {
        b"moov" => false,
        b"free" => true,
        _ => return Err(Mp4Error::NotFragmented("no init moov")),
    };
    if init.size > MAX_INIT_BYTES {
        return Err(Mp4Error::NotFragmented("init moov too large"));
    }
    let payload = read_payload(r, &init)?;
    if finalized && payload.get(4..8) != Some(b"mvhd".as_slice()) {
        return Err(Mp4Error::NotFragmented("no init moov"));
    }
    let (mut tracks, trex, creation_time_unix) = parse_init(&payload)?;

    let mut scan = FragmentedScan {
        tracks: Vec::new(),
        creation_time_unix,
        init_moov: (!finalized).then_some(init.offset),
        finalized,
        moofs: Vec::new(),
        fragments: 0,
        data_end: init.end(),
        file_len,
        stopped: None,
    };
    let mut pos = init.end();
    loop {
        let header = match read_header(r, pos, file_len) {
            Ok(Some(h)) => h,
            Ok(None) => {
                if pos < file_len {
                    scan.stopped = Some(format!("truncated box at {pos}"));
                }
                break;
            }
            Err(e) => {
                scan.stopped = Some(e.to_string());
                break;
            }
        };
        let is_fragment = match &header.kind {
            b"moof" => true,
            b"free" | b"skip" => {
                // A fragment that finalising already turned into `free`?
                let mut peek = [0u8; 8];
                header.size >= header.header_len.saturating_add(8) && {
                    r.seek(SeekFrom::Start(header.payload_start()))?;
                    r.read_exact(&mut peek)?;
                    peek.get(4..8) == Some(b"mfhd".as_slice())
                }
            }
            b"moov" => {
                scan.stopped = Some("final moov".into());
                break;
            }
            _ => false,
        };
        if !is_fragment {
            pos = header.end();
            continue;
        }
        match read_fragment(r, &header, file_len, &mut tracks, &trex) {
            Ok(end) => {
                if &header.kind == b"moof" {
                    scan.moofs.push(header.offset);
                }
                scan.fragments = scan.fragments.saturating_add(1);
                scan.data_end = end;
                pos = end;
            }
            Err(e) => {
                scan.stopped = Some(format!("fragment at {}: {e}", header.offset));
                break;
            }
        }
    }
    scan.tracks = tracks;
    Ok(scan)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BoxHeader {
    pub(crate) kind: [u8; 4],
    pub(crate) offset: u64,
    pub(crate) header_len: u64,
    /// Whole box, header included.
    pub(crate) size: u64,
}

impl BoxHeader {
    pub(crate) fn end(&self) -> u64 {
        self.offset.saturating_add(self.size)
    }

    fn payload_start(&self) -> u64 {
        self.offset.saturating_add(self.header_len)
    }
}

/// The box header at `offset`; `None` if the box doesn't fit in the file.
pub(crate) fn read_header<R: Read + Seek>(
    r: &mut R,
    offset: u64,
    file_len: u64,
) -> Result<Option<BoxHeader>, Mp4Error> {
    if offset.checked_add(8).is_none_or(|e| e > file_len) {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(offset))?;
    let mut h = [0u8; 8];
    r.read_exact(&mut h)?;
    let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    let kind = [h[4], h[5], h[6], h[7]];
    let (header_len, size) = match size32 {
        0 => (8, file_len.saturating_sub(offset)),
        1 => {
            if offset.checked_add(16).is_none_or(|e| e > file_len) {
                return Ok(None);
            }
            let mut l = [0u8; 8];
            r.read_exact(&mut l)?;
            (16, u64::from_be_bytes(l))
        }
        n => (8, u64::from(n)),
    };
    if size < header_len {
        return Err(Mp4Error::Malformed(format!("box size {size} at {offset}")));
    }
    if offset.checked_add(size).is_none_or(|e| e > file_len) {
        return Ok(None);
    }
    Ok(Some(BoxHeader {
        kind,
        offset,
        header_len,
        size,
    }))
}

pub(crate) fn read_payload<R: Read + Seek>(r: &mut R, h: &BoxHeader) -> Result<Vec<u8>, Mp4Error> {
    let len = usize::try_from(h.size.saturating_sub(h.header_len))
        .map_err(|_| Mp4Error::TooLarge("box"))?;
    r.seek(SeekFrom::Start(h.payload_start()))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read the fragment whose `moof` (or flipped `free`) is `moof`, followed by its `mdat`.
/// Samples are added to `tracks` only if the whole fragment is valid. Returns the end of
/// the `mdat`.
fn read_fragment<R: Read + Seek>(
    r: &mut R,
    moof: &BoxHeader,
    file_len: u64,
    tracks: &mut [ScannedTrack],
    trex: &[Trex],
) -> Result<u64, Mp4Error> {
    if moof.size > MAX_MOOF_BYTES {
        return Err(Mp4Error::Malformed("moof too large".into()));
    }
    let payload = read_payload(r, moof)?;
    let mdat = read_header(r, moof.end(), file_len)?
        .ok_or_else(|| Mp4Error::Malformed("incomplete mdat".into()))?;
    if &mdat.kind != b"mdat" {
        return Err(Mp4Error::Malformed("no mdat after moof".into()));
    }
    let data = (mdat.payload_start(), mdat.end());
    let parsed = parse_moof(&payload, moof.offset, data, tracks, trex)?;
    for (ti, samples, runs) in parsed {
        let t = tracks
            .get_mut(ti)
            .ok_or(Mp4Error::Internal("track index"))?;
        t.samples.extend(samples);
        t.runs.extend(runs);
    }
    Ok(mdat.end())
}

type ParsedTraf = (usize, Vec<ScannedSample>, Vec<(u64, u32)>);

fn parse_moof(
    moof: &[u8],
    moof_offset: u64,
    data: (u64, u64),
    tracks: &[ScannedTrack],
    trex: &[Trex],
) -> Result<Vec<ParsedTraf>, Mp4Error> {
    let mut out: Vec<ParsedTraf> = Vec::new();
    let mut prev_traf_end = moof_offset;
    let mut first_traf = true;
    for (kind, body) in children(moof)? {
        if &kind != b"traf" {
            continue;
        }
        let mut tfhd: Option<(u32, Trex, u64)> = None;
        let mut base_decode: Option<u64> = None;
        let mut samples: Vec<ScannedSample> = Vec::new();
        let mut runs: Vec<(u64, u32)> = Vec::new();
        let mut track_index = None;
        let mut next_data: Option<u64> = None;
        for (k, b) in children(body)? {
            match &k {
                b"tfhd" => {
                    let mut c = Cur::new(b);
                    let flags = c.full_box()?.1;
                    let id = c.u32()?;
                    let ti = tracks
                        .iter()
                        .position(|t| t.track_id == id)
                        .ok_or_else(|| Mp4Error::Malformed(format!("unknown track {id}")))?;
                    let mut d = trex
                        .iter()
                        .find(|t| t.track_id == id)
                        .copied()
                        .unwrap_or_default();
                    let base = if flags & 0x1 != 0 {
                        c.u64()?
                    } else if flags & TFHD_DEFAULT_BASE_IS_MOOF != 0 || first_traf {
                        moof_offset
                    } else {
                        prev_traf_end
                    };
                    if flags & 0x2 != 0 {
                        c.u32()?; // sample description index: one entry per track
                    }
                    if flags & 0x8 != 0 {
                        d.duration = c.u32()?;
                    }
                    if flags & 0x10 != 0 {
                        d.size = c.u32()?;
                    }
                    if flags & 0x20 != 0 {
                        d.flags = c.u32()?;
                    }
                    track_index = Some(ti);
                    tfhd = Some((id, d, base));
                }
                b"tfdt" => {
                    let mut c = Cur::new(b);
                    let version = c.full_box()?.0;
                    base_decode = Some(if version == 1 {
                        c.u64()?
                    } else {
                        u64::from(c.u32()?)
                    });
                }
                b"trun" => {
                    let (_, defaults, base) =
                        tfhd.ok_or_else(|| Mp4Error::Malformed("trun before tfhd".into()))?;
                    let ti = track_index.ok_or(Mp4Error::Internal("track index"))?;
                    let decode = match (samples.last(), base_decode) {
                        (Some(s), _) => s.decode_time.saturating_add(u64::from(s.duration)),
                        (None, Some(d)) => d,
                        // No tfdt: continue from the track's previous fragment.
                        (None, None) => tracks
                            .get(ti)
                            .and_then(|t| t.samples.last())
                            .map(|s| s.decode_time.saturating_add(u64::from(s.duration)))
                            .unwrap_or(0),
                    };
                    let start_default = next_data.unwrap_or(base);
                    let (run, end) = parse_trun(b, base, start_default, decode, &defaults, data)?;
                    if let Some(first) = run.first() {
                        let count =
                            u32::try_from(run.len()).map_err(|_| Mp4Error::TooLarge("samples"))?;
                        runs.push((first.0, count));
                    }
                    samples.extend(run.into_iter().map(|(_, s)| s));
                    next_data = Some(end);
                }
                _ => {}
            }
        }
        let ti = track_index.ok_or_else(|| Mp4Error::Malformed("traf without tfhd".into()))?;
        if out.iter().any(|(i, _, _)| *i == ti) {
            return Err(Mp4Error::Malformed("track twice in one fragment".into()));
        }
        // Decode times must not go backwards across fragments.
        if let (Some(first), Some(prev)) = (
            samples.first(),
            tracks.get(ti).and_then(|t| t.samples.last()),
        ) && first.decode_time < prev.decode_time
        {
            return Err(Mp4Error::Malformed("decode time went backwards".into()));
        }
        prev_traf_end = next_data.unwrap_or(prev_traf_end);
        first_traf = false;
        out.push((ti, samples, runs));
    }
    if out.is_empty() {
        return Err(Mp4Error::Malformed("fragment without tracks".into()));
    }
    Ok(out)
}

/// Parse one `trun`. Returns each sample with its file offset, and the end of its data.
fn parse_trun(
    body: &[u8],
    base: u64,
    start_default: u64,
    mut decode: u64,
    d: &Trex,
    data: (u64, u64),
) -> Result<(Vec<(u64, ScannedSample)>, u64), Mp4Error> {
    let mut c = Cur::new(body);
    let (version, flags) = c.full_box()?;
    let count = c.u32()?;
    let mut pos = if flags & TRUN_DATA_OFFSET != 0 {
        let off = i64::from(c.i32()?);
        i64::try_from(base)
            .ok()
            .and_then(|b| b.checked_add(off))
            .and_then(|v| u64::try_from(v).ok())
            .ok_or_else(|| Mp4Error::Malformed("data offset".into()))?
    } else {
        start_default
    };
    let first_flags = if flags & TRUN_FIRST_SAMPLE_FLAGS != 0 {
        Some(c.u32()?)
    } else {
        None
    };
    let per_sample = [TRUN_DURATION, TRUN_SIZE, TRUN_FLAGS, TRUN_CTS]
        .iter()
        .filter(|f| flags & **f != 0)
        .count()
        .saturating_mul(4);
    // The count can't promise more samples than the box holds.
    let max = if per_sample == 0 {
        MAX_RUN_SAMPLES
    } else {
        c.remaining()
            .checked_div(per_sample)
            .unwrap_or(0)
            .min(MAX_RUN_SAMPLES)
    };
    let count = usize::try_from(count).map_err(|_| Mp4Error::TooLarge("samples"))?;
    if count > max {
        return Err(Mp4Error::Malformed("trun sample count".into()));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let duration = if flags & TRUN_DURATION != 0 {
            c.u32()?
        } else {
            d.duration
        };
        let size = if flags & TRUN_SIZE != 0 {
            c.u32()?
        } else {
            d.size
        };
        let sample_flags = if flags & TRUN_FLAGS != 0 {
            c.u32()?
        } else if i == 0
            && let Some(f) = first_flags
        {
            f
        } else {
            d.flags
        };
        let cts_offset = if flags & TRUN_CTS != 0 {
            let raw = c.u32()?;
            if version == 0 {
                i32::try_from(raw).map_err(|_| Mp4Error::Malformed("composition offset".into()))?
            } else {
                i32::from_be_bytes(raw.to_be_bytes())
            }
        } else {
            0
        };
        let end = pos
            .checked_add(u64::from(size))
            .ok_or_else(|| Mp4Error::Malformed("sample offset".into()))?;
        if pos < data.0 || end > data.1 {
            return Err(Mp4Error::Malformed("sample outside its mdat".into()));
        }
        out.push((
            pos,
            ScannedSample {
                decode_time: decode,
                duration,
                cts_offset,
                size,
                sync: sample_flags & NON_SYNC == 0,
            },
        ));
        pos = end;
        decode = decode
            .checked_add(u64::from(duration))
            .ok_or_else(|| Mp4Error::Malformed("decode time".into()))?;
    }
    Ok((out, pos))
}

/// Tracks, `trex` defaults and creation time from an init `moov` payload.
fn parse_init(moov: &[u8]) -> Result<(Vec<ScannedTrack>, Vec<Trex>, u64), Mp4Error> {
    let mut tracks = Vec::new();
    let mut trex = Vec::new();
    let mut created = 0u64;
    let mut has_mvex = false;
    for (kind, body) in children(moov)? {
        match &kind {
            b"mvhd" => {
                let mut c = Cur::new(body);
                let version = c.full_box()?.0;
                let t = if version == 1 {
                    c.u64()?
                } else {
                    u64::from(c.u32()?)
                };
                created = t.saturating_sub(MP4_EPOCH_OFFSET);
            }
            b"trak" => {
                if tracks.len() >= MAX_TRACKS {
                    return Err(Mp4Error::Malformed("too many tracks".into()));
                }
                tracks.push(parse_trak(body)?);
            }
            b"mvex" => {
                has_mvex = true;
                for (k, b) in children(body)? {
                    if &k == b"trex" {
                        let mut c = Cur::new(b);
                        c.full_box()?;
                        let track_id = c.u32()?;
                        c.u32()?; // default sample description index
                        trex.push(Trex {
                            track_id,
                            duration: c.u32()?,
                            size: c.u32()?,
                            flags: c.u32()?,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if !has_mvex {
        return Err(Mp4Error::NotFragmented("no mvex in the moov"));
    }
    if tracks.is_empty() {
        return Err(Mp4Error::NoTracks);
    }
    Ok((tracks, trex, created))
}

fn parse_trak(trak: &[u8]) -> Result<ScannedTrack, Mp4Error> {
    let mut track_id = None;
    let mut enabled = true;
    let mut timescale = None;
    let mut handler = None;
    let mut name = None;
    let mut entry = None;
    for (kind, body) in children(trak)? {
        match &kind {
            b"tkhd" => {
                let mut c = Cur::new(body);
                let (version, flags) = c.full_box()?;
                c.skip(if version == 1 { 16 } else { 8 })?;
                track_id = Some(c.u32()?);
                enabled = flags & 1 != 0;
            }
            b"mdia" => {
                for (k, b) in children(body)? {
                    match &k {
                        b"mdhd" => {
                            let mut c = Cur::new(b);
                            let version = c.full_box()?.0;
                            c.skip(if version == 1 { 16 } else { 8 })?;
                            timescale = Some(c.u32()?);
                        }
                        b"hdlr" => {
                            let mut c = Cur::new(b);
                            c.full_box()?;
                            c.skip(4)?;
                            handler = Some(c.fourcc()?);
                            c.skip(12)?;
                            let rest = c.rest();
                            let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
                            let n = String::from_utf8_lossy(rest.get(..end).unwrap_or_default());
                            name = (!n.is_empty()).then(|| n.into_owned());
                        }
                        b"minf" => {
                            entry = find_sample_entry(b)?;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let track_id = track_id.ok_or_else(|| Mp4Error::Malformed("trak without tkhd".into()))?;
    let timescale = timescale
        .filter(|t| *t > 0)
        .ok_or_else(|| Mp4Error::Malformed("trak without timescale".into()))?;
    let (entry_kind, entry_body) =
        entry.ok_or_else(|| Mp4Error::Malformed("trak without sample entry".into()))?;
    let info = match handler {
        Some(h) if &h == b"vide" => parse_video_entry(&entry_kind, entry_body)?,
        Some(h) if &h == b"soun" => parse_audio_entry(&entry_kind, entry_body, timescale)?,
        _ => return Err(Mp4Error::Malformed("unknown handler".into())),
    };
    info.validate()
        .map_err(|e| Mp4Error::Malformed(format!("track {track_id}: {e}")))?;
    Ok(ScannedTrack {
        info,
        name,
        enabled,
        track_id,
        timescale,
        samples: Vec::new(),
        runs: Vec::new(),
    })
}

fn find_sample_entry(minf: &[u8]) -> Result<Option<Child<'_>>, Mp4Error> {
    for (k, b) in children(minf)? {
        if &k != b"stbl" {
            continue;
        }
        for (k2, b2) in children(b)? {
            if &k2 == b"stsd" {
                let mut c = Cur::new(b2);
                c.full_box()?;
                if c.u32()? == 0 {
                    return Ok(None);
                }
                return Ok(children(c.rest())?.into_iter().next());
            }
        }
    }
    Ok(None)
}

fn codec_data(b: &[u8]) -> Result<Bytes, Mp4Error> {
    if b.len() > MAX_CODEC_DATA_BYTES {
        return Err(Mp4Error::Malformed("codec data too large".into()));
    }
    Ok(Bytes::copy_from_slice(b))
}

fn parse_video_entry(kind: &[u8; 4], body: &[u8]) -> Result<TrackInfo, Mp4Error> {
    let (codec, config): (Codec, &[u8; 4]) = match kind {
        b"avc1" | b"avc3" => (Codec::H264, b"avcC"),
        b"hvc1" | b"hev1" => (Codec::H265, b"hvcC"),
        b"av01" => (Codec::Av1, b"av1C"),
        _ => return Err(Mp4Error::Malformed("unknown video sample entry".into())),
    };
    let mut c = Cur::new(body);
    c.skip(24)?;
    let width = u32::from(c.u16()?);
    let height = u32::from(c.u16()?);
    c.skip(50)?;
    let mut data = None;
    let mut colorimetry = None;
    for (k, b) in children(c.rest())? {
        if &k == config {
            data = Some(codec_data(b)?);
        } else if &k == b"colr" && b.get(0..4) == Some(b"nclx".as_slice()) {
            let mut cc = Cur::new(b);
            cc.skip(4)?;
            colorimetry = Some(Colorimetry {
                primaries: cc.u16()?,
                transfer: cc.u16()?,
                matrix: cc.u16()?,
                full_range: cc.u8()? & 0x80 != 0,
            });
        }
    }
    Ok(TrackInfo {
        codec,
        codec_data: data.ok_or_else(|| Mp4Error::Malformed("no codec configuration".into()))?,
        params: TrackParams::Video(VideoParams {
            width,
            height,
            colorimetry,
        }),
        bitrate: None,
    })
}

fn parse_audio_entry(kind: &[u8; 4], body: &[u8], timescale: u32) -> Result<TrackInfo, Mp4Error> {
    let mut c = Cur::new(body);
    c.skip(8)?;
    let version = c.u16()?;
    if version != 0 {
        return Err(Mp4Error::Malformed("unsupported audio sample entry".into()));
    }
    c.skip(6)?;
    let channels = c.u16()?;
    c.skip(10)?;
    let params = TrackParams::Audio(AudioParams {
        sample_rate: timescale,
        channels,
    });
    let kids = children(c.rest())?;
    match kind {
        b"mp4a" => {
            let esds = kids
                .iter()
                .find(|(k, _)| k == b"esds")
                .map(|(_, b)| *b)
                .ok_or_else(|| Mp4Error::Malformed("mp4a without esds".into()))?;
            let (data, bitrate) = parse_esds(esds)?;
            Ok(TrackInfo {
                codec: Codec::Aac,
                codec_data: data,
                params,
                bitrate: (bitrate > 0).then_some(bitrate),
            })
        }
        b"Opus" => {
            let dops = kids
                .iter()
                .find(|(k, _)| k == b"dOps")
                .map(|(_, b)| *b)
                .ok_or_else(|| Mp4Error::Malformed("Opus without dOps".into()))?;
            Ok(TrackInfo {
                codec: Codec::Opus,
                codec_data: opus_head(dops)?,
                params,
                bitrate: None,
            })
        }
        _ => Err(Mp4Error::Malformed("unknown audio sample entry".into())),
    }
}

/// MPEG-4 descriptor: (tag, body).
fn descriptor<'a>(c: &mut Cur<'a>) -> Result<(u8, &'a [u8]), Mp4Error> {
    let tag = c.u8()?;
    let mut len: usize = 0;
    for _ in 0..4 {
        let b = c.u8()?;
        len = (len << 7) | usize::from(b & 0x7F);
        if b & 0x80 == 0 {
            break;
        }
    }
    Ok((tag, c.take(len)?))
}

/// AudioSpecificConfig and average bitrate from an `esds` box.
fn parse_esds(b: &[u8]) -> Result<(Bytes, u32), Mp4Error> {
    let mut c = Cur::new(b);
    c.full_box()?;
    let (tag, es) = descriptor(&mut c)?;
    if tag != 0x03 {
        return Err(Mp4Error::Malformed("esds without ES descriptor".into()));
    }
    let mut c = Cur::new(es);
    c.skip(2)?;
    let flags = c.u8()?;
    if flags & 0x80 != 0 {
        c.skip(2)?;
    }
    if flags & 0x40 != 0 {
        let n = c.u8()?;
        c.skip(usize::from(n))?;
    }
    if flags & 0x20 != 0 {
        c.skip(2)?;
    }
    while c.remaining() > 0 {
        let (tag, body) = descriptor(&mut c)?;
        if tag != 0x04 {
            continue;
        }
        let mut d = Cur::new(body);
        if d.u8()? != 0x40 {
            return Err(Mp4Error::Malformed("esds is not MPEG-4 audio".into()));
        }
        d.skip(1 + 3 + 4)?;
        let avg = d.u32()?;
        while d.remaining() > 0 {
            let (t, dsi) = descriptor(&mut d)?;
            if t == 0x05 {
                return Ok((codec_data(dsi)?, avg));
            }
        }
    }
    Err(Mp4Error::Malformed("esds without decoder config".into()))
}

/// Rebuild the `OpusHead` packet (RFC 7845, little-endian) from a `dOps` box.
fn opus_head(b: &[u8]) -> Result<Bytes, Mp4Error> {
    let mut c = Cur::new(b);
    c.skip(1)?;
    let channels = c.u8()?;
    let pre_skip = c.u16()?;
    let rate = c.u32()?;
    let gain = c.u16()?;
    let family = c.u8()?;
    let mut head = Vec::with_capacity(32);
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(channels);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&gain.to_le_bytes());
    head.push(family);
    if family != 0 {
        head.extend_from_slice(c.take(usize::from(channels).saturating_add(2))?);
    }
    Ok(Bytes::from(head))
}

/// Child boxes of a container payload. Errors if a child overruns the container.
pub(crate) fn children(d: &[u8]) -> Result<Vec<Child<'_>>, Mp4Error> {
    let mut out = Vec::new();
    let mut c = Cur::new(d);
    while c.remaining() > 0 {
        let size32 = c.u32()?;
        let kind = c.fourcc()?;
        let (header, size) = match size32 {
            0 => (8usize, c.remaining().saturating_add(8)),
            1 => {
                let l = usize::try_from(c.u64()?).map_err(|_| Mp4Error::TooLarge("box"))?;
                (16, l)
            }
            n => (
                8,
                usize::try_from(n).map_err(|_| Mp4Error::TooLarge("box"))?,
            ),
        };
        let body_len = size
            .checked_sub(header)
            .ok_or_else(|| Mp4Error::Malformed("box smaller than its header".into()))?;
        out.push((kind, c.take(body_len)?));
    }
    Ok(out)
}

/// Bounds-checked big-endian reader over a byte slice.
pub(crate) struct Cur<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    pub(crate) fn new(d: &'a [u8]) -> Cur<'a> {
        Cur { d, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.d.len().saturating_sub(self.pos)
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Mp4Error> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| Mp4Error::Malformed("box overrun".into()))?;
        let s = self
            .d
            .get(self.pos..end)
            .ok_or_else(|| Mp4Error::Malformed("box overrun".into()))?;
        self.pos = end;
        Ok(s)
    }

    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let s = self.d.get(self.pos..).unwrap_or_default();
        self.pos = self.d.len();
        s
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<(), Mp4Error> {
        self.take(n).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Mp4Error> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Mp4Error> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Mp4Error> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Mp4Error> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, Mp4Error> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Mp4Error> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(crate) fn fourcc(&mut self) -> Result<[u8; 4], Mp4Error> {
        self.array()
    }

    /// Version and flags of a full box.
    pub(crate) fn full_box(&mut self) -> Result<(u8, u32), Mp4Error> {
        let v = self.u32()?;
        let [version, a, b, c] = v.to_be_bytes();
        Ok((version, u32::from_be_bytes([0, a, b, c])))
    }
}
