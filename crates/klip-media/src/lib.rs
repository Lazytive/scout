//! Shared media types for Scout: codecs, track descriptions and encoded packets.
//!
//! Pure data, no I/O. Timestamps are nanoseconds in a signed 64-bit integer so that
//! differences and pre-roll can be represented without wrapping.

use bytes::Bytes;

pub mod av1;
mod drift;
mod grid;
pub mod h264;

pub use drift::{Aligned, DriftAligner};
pub use grid::FrameGrid;

/// Nanoseconds.
pub type Nanos = i64;

/// Nanoseconds per second.
pub const NANOS_PER_SEC: Nanos = 1_000_000_000;

/// Nanoseconds per millisecond.
pub const NANOS_PER_MILLI: Nanos = 1_000_000;

/// Largest single encoded packet Scout accepts (64 MiB). Anything bigger is treated as
/// corrupt input rather than allocated.
pub const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;

/// Largest codec configuration blob (avcC, hvcC, …) Scout accepts.
pub const MAX_CODEC_DATA_BYTES: usize = 64 * 1024;

/// Compressed formats Scout stores and muxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    H265,
    Av1,
    Aac,
    Opus,
}

impl Codec {
    /// Whether this is a video or audio codec.
    pub fn kind(self) -> TrackKind {
        match self {
            Codec::H264 | Codec::H265 | Codec::Av1 => TrackKind::Video,
            Codec::Aac | Codec::Opus => TrackKind::Audio,
        }
    }

    /// Short lowercase name used in logs, CLI flags and file metadata.
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::H265 => "h265",
            Codec::Av1 => "av1",
            Codec::Aac => "aac",
            Codec::Opus => "opus",
        }
    }

    /// Parse a codec name as produced by [`Codec::name`] (case-insensitive; `hevc` is
    /// accepted for H.265).
    pub fn from_name(name: &str) -> Option<Codec> {
        match name.to_ascii_lowercase().as_str() {
            "h264" | "avc" => Some(Codec::H264),
            "h265" | "hevc" => Some(Codec::H265),
            "av1" => Some(Codec::Av1),
            "aac" => Some(Codec::Aac),
            "opus" => Some(Codec::Opus),
            _ => None,
        }
    }
}

/// Track type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    Video,
    Audio,
}

/// Colour description using the code points of ISO/IEC 23091-2 (same numbers as H.264
/// VUI and the MP4 `colr`/`nclx` box).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Colorimetry {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

impl Colorimetry {
    /// BT.709, limited range: what SDR game capture is encoded as.
    pub const BT709: Colorimetry = Colorimetry {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };
}

/// Parameters of a video track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoParams {
    pub width: u32,
    pub height: u32,
    /// `None` when the encoder didn't say; muxers then omit colour boxes.
    pub colorimetry: Option<Colorimetry>,
}

/// Parameters of an audio track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioParams {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Video or audio parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackParams {
    Video(VideoParams),
    Audio(AudioParams),
}

/// Everything needed to decode a track's packets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
    pub codec: Codec,
    /// Codec configuration exactly as stored in MP4: the `avcC` / `hvcC` / `av1C` box
    /// payload, or the AAC `AudioSpecificConfig`.
    pub codec_data: Bytes,
    pub params: TrackParams,
    /// Average bitrate in bits per second, if known.
    pub bitrate: Option<u32>,
}

/// Why a [`TrackInfo`] was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackInfoError {
    ZeroDimension,
    ZeroSampleRate,
    ZeroChannels,
    KindMismatch,
    CodecDataTooLarge(usize),
    MissingCodecData,
}

impl std::fmt::Display for TrackInfoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackInfoError::ZeroDimension => write!(f, "video width or height is zero"),
            TrackInfoError::ZeroSampleRate => write!(f, "audio sample rate is zero"),
            TrackInfoError::ZeroChannels => write!(f, "audio channel count is zero"),
            TrackInfoError::KindMismatch => {
                write!(f, "codec and parameters disagree on track kind")
            }
            TrackInfoError::CodecDataTooLarge(n) => write!(f, "codec data too large ({n} bytes)"),
            TrackInfoError::MissingCodecData => write!(f, "codec data missing"),
        }
    }
}

impl std::error::Error for TrackInfoError {}

impl TrackInfo {
    /// The track kind implied by the codec.
    pub fn kind(&self) -> TrackKind {
        self.codec.kind()
    }

    /// Check the description is internally consistent and within size limits.
    pub fn validate(&self) -> Result<(), TrackInfoError> {
        if self.codec_data.len() > MAX_CODEC_DATA_BYTES {
            return Err(TrackInfoError::CodecDataTooLarge(self.codec_data.len()));
        }
        match (&self.params, self.codec.kind()) {
            (TrackParams::Video(v), TrackKind::Video) => {
                if v.width == 0 || v.height == 0 {
                    return Err(TrackInfoError::ZeroDimension);
                }
                if self.codec_data.is_empty() {
                    return Err(TrackInfoError::MissingCodecData);
                }
            }
            (TrackParams::Audio(a), TrackKind::Audio) => {
                if a.sample_rate == 0 {
                    return Err(TrackInfoError::ZeroSampleRate);
                }
                if a.channels == 0 {
                    return Err(TrackInfoError::ZeroChannels);
                }
                if self.codec == Codec::Aac && self.codec_data.is_empty() {
                    return Err(TrackInfoError::MissingCodecData);
                }
            }
            _ => return Err(TrackInfoError::KindMismatch),
        }
        Ok(())
    }
}

/// One encoded access unit (a video frame or an audio frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// Presentation time.
    pub pts: Nanos,
    /// Decode time; equal to `pts` for streams without reordering.
    pub dts: Nanos,
    /// Duration, or 0 when unknown.
    pub duration: Nanos,
    /// True for random-access points (IDR frames; every audio packet).
    pub keyframe: bool,
    pub data: Bytes,
}

impl Packet {
    /// End of this packet on the decode timeline (`dts + duration`), saturating.
    pub fn dts_end(&self) -> Nanos {
        self.dts.saturating_add(self.duration.max(0))
    }
}

/// Convert nanoseconds to whole milliseconds (truncating toward zero).
pub fn nanos_to_millis(n: Nanos) -> i64 {
    n.checked_div(NANOS_PER_MILLI).unwrap_or(0)
}

/// Convert milliseconds to nanoseconds, saturating.
pub fn millis_to_nanos(ms: i64) -> Nanos {
    ms.saturating_mul(NANOS_PER_MILLI)
}

/// Parse a human duration such as `120s`, `2m`, `1m30s`, `500ms` or a bare number of
/// seconds. Returns `None` for anything else, for zero and for absurd values (> 24 h).
pub fn parse_duration(text: &str) -> Option<Nanos> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        let secs: i64 = text.parse().ok()?;
        return finish_duration(secs.checked_mul(NANOS_PER_SEC)?);
    }
    let mut total: Nanos = 0;
    let mut rest = text;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let (num, tail) = rest.split_at(digits);
        let value: i64 = num.parse().ok()?;
        let unit_len = tail.bytes().take_while(u8::is_ascii_alphabetic).count();
        let (unit, tail) = tail.split_at(unit_len);
        let scale = match unit {
            "ms" => NANOS_PER_MILLI,
            "s" => NANOS_PER_SEC,
            "m" | "min" => NANOS_PER_SEC.checked_mul(60)?,
            "h" => NANOS_PER_SEC.checked_mul(3600)?,
            _ => return None,
        };
        total = total.checked_add(value.checked_mul(scale)?)?;
        rest = tail;
    }
    finish_duration(total)
}

fn finish_duration(n: Nanos) -> Option<Nanos> {
    let day = NANOS_PER_SEC.checked_mul(86_400)?;
    if n <= 0 || n > day { None } else { Some(n) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("120s"), Some(120 * NANOS_PER_SEC));
        assert_eq!(parse_duration("2m"), Some(120 * NANOS_PER_SEC));
        assert_eq!(parse_duration("1m30s"), Some(90 * NANOS_PER_SEC));
        assert_eq!(parse_duration("500ms"), Some(500 * NANOS_PER_MILLI));
        assert_eq!(parse_duration("30"), Some(30 * NANOS_PER_SEC));
        assert_eq!(parse_duration(" 45s "), Some(45 * NANOS_PER_SEC));
    }

    #[test]
    fn durations_reject_garbage() {
        for bad in [
            "",
            "0",
            "0s",
            "s",
            "-5s",
            "5x",
            "1.5s",
            "99999999999999999999s",
            "25h",
            "1s2",
            "ms30",
        ] {
            assert_eq!(parse_duration(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn track_info_validation() {
        let mut info = TrackInfo {
            codec: Codec::H264,
            codec_data: Bytes::from_static(&[1, 2, 3]),
            params: TrackParams::Video(VideoParams {
                width: 1920,
                height: 1080,
                colorimetry: None,
            }),
            bitrate: None,
        };
        assert_eq!(info.validate(), Ok(()));
        info.params = TrackParams::Audio(AudioParams {
            sample_rate: 48_000,
            channels: 2,
        });
        assert_eq!(info.validate(), Err(TrackInfoError::KindMismatch));
        info.codec = Codec::Aac;
        assert_eq!(info.validate(), Ok(()));
        info.codec_data = Bytes::from(vec![0u8; MAX_CODEC_DATA_BYTES + 1]);
        assert!(matches!(
            info.validate(),
            Err(TrackInfoError::CodecDataTooLarge(_))
        ));
    }

    #[test]
    fn codec_names_round_trip() {
        for c in [
            Codec::H264,
            Codec::H265,
            Codec::Av1,
            Codec::Aac,
            Codec::Opus,
        ] {
            assert_eq!(Codec::from_name(c.name()), Some(c));
        }
        assert_eq!(Codec::from_name("HEVC"), Some(Codec::H265));
        assert_eq!(Codec::from_name("mpeg2"), None);
    }
}
