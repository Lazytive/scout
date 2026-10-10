//! What the library shows about a clip, read from the `moov` box alone (no sample data):
//! duration, creation time, the video's codec, size and frame rate, the number of audio
//! tracks and the chapter (bookmark) times. Works on any MP4: Klip's faststart clips,
//! files with the `moov` at the end, finalised and unfinalised recordings, and files
//! from other programs.
//!
//! The input is hostile: box sizes are checked against the file and their parent, the
//! `moov` read is capped, and a track or `udta` that doesn't parse is skipped rather
//! than failing the whole file.

use std::io::{Read, Seek, SeekFrom};

use crate::Mp4Error;
use crate::moov::MP4_EPOCH_OFFSET;
use crate::read::{Cur, children, read_header, read_payload};

/// Largest `moov` read (an hour at 240 fps needs a few MB).
pub const MAX_INFO_MOOV_BYTES: u64 = 64 << 20;
/// Most top-level boxes walked looking for the `moov`.
const MAX_TOP_BOXES: usize = 100_000;
/// Most tracks looked at.
const MAX_INFO_TRACKS: usize = 64;
/// Most chapters returned (the `chpl` count is one byte).
const MAX_INFO_CHAPTERS: usize = 255;
/// Highest frame rate reported; anything above is nonsense.
const MAX_FPS: f64 = 1000.0;

/// The video track of an MP4.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoSummary {
    /// Display name of the codec (`H.264`, `HEVC`, `AV1`, `VP9`, or the sample entry's
    /// four letters).
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// Average frame rate (samples / track duration); `None` when unknown (fragmented).
    pub fps: Option<f64>,
}

/// What [`probe`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mp4Info {
    /// Movie duration; `None` when the file doesn't say (fragmented, or zero).
    pub duration_ms: Option<u64>,
    /// Creation time, seconds since the Unix epoch; 0 when not set.
    pub creation_time_unix: u64,
    /// The first video track.
    pub video: Option<VideoSummary>,
    pub audio_tracks: u32,
    /// The audio tracks' handler names in track order (Klip writes `Mix`, `Game`,
    /// `Microphone`, ...; empty for an unnamed track).
    pub audio_names: Vec<String>,
    /// Chapter starts (Klip's bookmarks), ms from the start.
    pub chapters_ms: Vec<u64>,
    /// The `moov` describes fragments (a recording that was never finalised).
    pub fragmented: bool,
}

/// Read what [`Mp4Info`] holds from an MP4.
pub fn probe<R: Read + Seek>(r: &mut R) -> Result<Mp4Info, Mp4Error> {
    let file_len = r.seek(SeekFrom::End(0))?;
    let mut pos = 0u64;
    let mut moov = None;
    for _ in 0..MAX_TOP_BOXES {
        let Some(h) = read_header(r, pos, file_len)? else {
            break;
        };
        if &h.kind == b"moov" {
            moov = Some(h);
            break;
        }
        pos = h.end();
    }
    let moov = moov.ok_or_else(|| Mp4Error::Malformed("no moov box".into()))?;
    if moov.size > MAX_INFO_MOOV_BYTES {
        return Err(Mp4Error::TooLarge("moov"));
    }
    let payload = read_payload(r, &moov)?;
    parse_moov(&payload)
}

fn parse_moov(moov: &[u8]) -> Result<Mp4Info, Mp4Error> {
    let mut info = Mp4Info::default();
    let mut tracks = 0usize;
    for (kind, body) in children(moov)? {
        match &kind {
            b"mvhd" => {
                let (created, duration_ms) = parse_mvhd(body)?;
                info.creation_time_unix = created;
                info.duration_ms = duration_ms;
            }
            b"trak" => {
                if tracks >= MAX_INFO_TRACKS {
                    continue;
                }
                tracks = tracks.saturating_add(1);
                match parse_trak(body) {
                    Some(Track::Video(v)) if info.video.is_none() => info.video = Some(v),
                    Some(Track::Audio(name)) => {
                        info.audio_tracks = info.audio_tracks.saturating_add(1);
                        info.audio_names.push(name);
                    }
                    _ => {}
                }
            }
            b"mvex" => info.fragmented = true,
            b"udta" => info.chapters_ms = parse_udta(body),
            _ => {}
        }
    }
    if info.fragmented && info.duration_ms == Some(0) {
        info.duration_ms = None;
    }
    if let Some(v) = &mut info.video
        && info.fragmented
    {
        v.fps = None;
    }
    Ok(info)
}

/// Creation time (Unix seconds, 0 if unset or before 1970) and duration in ms.
fn parse_mvhd(body: &[u8]) -> Result<(u64, Option<u64>), Mp4Error> {
    let mut c = Cur::new(body);
    let version = c.full_box()?.0;
    let (created, timescale, duration) = if version == 1 {
        let created = c.u64()?;
        c.skip(8)?;
        (created, c.u32()?, c.u64()?)
    } else {
        let created = u64::from(c.u32()?);
        c.skip(4)?;
        let timescale = c.u32()?;
        let d = c.u32()?;
        // All ones: "unknown" in version 0.
        let d = if d == u32::MAX { 0 } else { u64::from(d) };
        (created, timescale, d)
    };
    let created = created.saturating_sub(MP4_EPOCH_OFFSET);
    Ok((created, to_ms(duration, timescale).filter(|d| *d > 0)))
}

fn to_ms(value: u64, timescale: u32) -> Option<u64> {
    if timescale == 0 || value == u64::MAX {
        return None;
    }
    let ms = u128::from(value)
        .checked_mul(1000)?
        .checked_div(u128::from(timescale))?;
    u64::try_from(ms).ok()
}

enum Track {
    Video(VideoSummary),
    /// With the handler name.
    Audio(String),
}

/// Longest handler name kept.
const MAX_HANDLER_NAME: usize = 64;

/// A track's kind and, for video, its summary; `None` for other tracks or ones that
/// don't parse.
fn parse_trak(trak: &[u8]) -> Option<Track> {
    let mdia = children(trak)
        .ok()?
        .into_iter()
        .find(|(k, _)| k == b"mdia")?
        .1;
    let mut timescale = 0u32;
    let mut duration = 0u64;
    let mut handler = None;
    let mut name = String::new();
    let mut stbl = None;
    for (k, b) in children(mdia).ok()? {
        match &k {
            b"mdhd" => {
                let mut c = Cur::new(b);
                let version = c.full_box().ok()?.0;
                if version == 1 {
                    c.skip(16).ok()?;
                    timescale = c.u32().ok()?;
                    duration = c.u64().ok()?;
                } else {
                    c.skip(8).ok()?;
                    timescale = c.u32().ok()?;
                    duration = u64::from(c.u32().ok()?);
                }
            }
            b"hdlr" => {
                let mut c = Cur::new(b);
                c.full_box().ok()?;
                c.skip(4).ok()?;
                handler = Some(c.fourcc().ok()?);
                // 12 reserved bytes, then the name (UTF-8, null-terminated).
                if c.skip(12).is_ok() {
                    let raw = c.rest();
                    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                    name = String::from_utf8_lossy(raw.get(..end).unwrap_or(&[]))
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(MAX_HANDLER_NAME)
                        .collect();
                }
            }
            b"minf" => {
                stbl = children(b)
                    .ok()?
                    .into_iter()
                    .find(|(k, _)| k == b"stbl")
                    .map(|(_, b)| b);
            }
            _ => {}
        }
    }
    match &handler? {
        b"soun" => Some(Track::Audio(name)),
        b"vide" => {
            let stbl = stbl?;
            let mut entry = None;
            let mut samples = 0u64;
            for (k, b) in children(stbl).ok()? {
                match &k {
                    b"stsd" => {
                        let mut c = Cur::new(b);
                        c.full_box().ok()?;
                        if c.u32().ok()? > 0 {
                            entry = children(c.rest()).ok()?.into_iter().next();
                        }
                    }
                    b"stsz" => {
                        let mut c = Cur::new(b);
                        c.full_box().ok()?;
                        c.skip(4).ok()?;
                        samples = u64::from(c.u32().ok()?);
                    }
                    b"stz2" => {
                        let mut c = Cur::new(b);
                        c.full_box().ok()?;
                        c.skip(4).ok()?;
                        samples = u64::from(c.u32().ok()?);
                    }
                    _ => {}
                }
            }
            let (kind, body) = entry?;
            // Visual sample entry: 6 reserved + data reference index, 16 bytes of
            // pre-defined/reserved, then width and height.
            let mut c = Cur::new(body);
            c.skip(24).ok()?;
            let width = u32::from(c.u16().ok()?);
            let height = u32::from(c.u16().ok()?);
            let fps = fps(samples, duration, timescale);
            Some(Track::Video(VideoSummary {
                codec: codec_name(&kind),
                width,
                height,
                fps,
            }))
        }
        _ => None,
    }
}

fn fps(samples: u64, duration: u64, timescale: u32) -> Option<f64> {
    if samples == 0 || duration == 0 || timescale == 0 {
        return None;
    }
    // u64 → f64 loses precision only far beyond any real sample count or duration.
    let f = samples as f64 * f64::from(timescale) / duration as f64;
    (f.is_finite() && f > 0.0 && f <= MAX_FPS).then_some(f)
}

fn codec_name(kind: &[u8; 4]) -> String {
    match kind {
        b"avc1" | b"avc3" => "H.264".into(),
        b"hvc1" | b"hev1" => "HEVC".into(),
        b"av01" => "AV1".into(),
        b"vp09" => "VP9".into(),
        b"mp4v" => "MPEG-4".into(),
        other => other
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() {
                    char::from(*b)
                } else {
                    '?'
                }
            })
            .collect(),
    }
}

/// Chapter starts from a Nero `chpl` box (what Klip writes for bookmarks); empty when
/// there is none or it doesn't parse.
fn parse_udta(udta: &[u8]) -> Vec<u64> {
    let Ok(kids) = children(udta) else {
        return Vec::new();
    };
    let Some((_, chpl)) = kids.into_iter().find(|(k, _)| k == b"chpl") else {
        return Vec::new();
    };
    parse_chpl(chpl).unwrap_or_default()
}

fn parse_chpl(body: &[u8]) -> Result<Vec<u64>, Mp4Error> {
    let mut c = Cur::new(body);
    let version = c.full_box()?.0;
    if version == 1 {
        c.skip(4)?;
    }
    let count = usize::from(c.u8()?);
    let mut out = Vec::with_capacity(count.min(MAX_INFO_CHAPTERS));
    for _ in 0..count {
        let at_100ns = c.u64()?;
        let len = usize::from(c.u8()?);
        c.skip(len)?;
        out.push(at_100ns.checked_div(10_000).unwrap_or(0));
    }
    out.sort_unstable();
    Ok(out)
}

#[cfg(test)]
#[path = "info_tests.rs"]
mod tests;
