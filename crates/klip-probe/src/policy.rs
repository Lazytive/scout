//! Which encoders Klip may ever use (AGENTS.md rules 5 and 6).
//!
//! The **allowlist** is the gate: only GPU vendor encoders, OS encoders (Media
//! Foundation) and, on Linux only, openh264 (meant to be Cisco's binary from the Flatpak
//! extension). The **denylist** names GPL and other software encoders that are present in
//! common GStreamer builds; it exists for the licence audit and as a second check. This
//! file is the only product source allowed to mention denied element names
//! (`cargo xtask audit` enforces that).

use klip_media::Codec;

/// Encoder implementation family; decides which properties apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncoderFamily {
    /// NVIDIA NVENC, Direct3D 11 input.
    NvencD3d11,
    /// NVIDIA NVENC, CUDA input.
    NvencCuda,
    /// AMD AMF.
    Amf,
    /// Intel Quick Sync.
    Qsv,
    /// Windows Media Foundation (hardware or Microsoft software MFT).
    MediaFoundation,
    /// Linux VA-API (AMD/Intel).
    VaApi,
    /// Cisco openh264 (Linux Flatpak extension only).
    OpenH264,
}

/// Who provides the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncoderVendor {
    Nvidia,
    Amd,
    Intel,
    /// OS-provided, vendor-neutral (Media Foundation, VA-API dispatch, openh264).
    Os,
}

/// Static description of an allowlisted encoder element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllowedEncoder {
    pub element: &'static str,
    pub codec: Codec,
    pub family: EncoderFamily,
    pub vendor: EncoderVendor,
    /// Only allowed on Linux.
    pub linux_only: bool,
}

const fn enc(
    element: &'static str,
    codec: Codec,
    family: EncoderFamily,
    vendor: EncoderVendor,
) -> AllowedEncoder {
    AllowedEncoder {
        element,
        codec,
        family,
        vendor,
        linux_only: false,
    }
}

use EncoderFamily as F;
use EncoderVendor as V;

/// Fixed-name allowlisted encoders, in preference order within each vendor. Media
/// Foundation encoders have per-device names (`mfh264enc`, `mfh264device1enc`, …) and
/// are matched by [`media_foundation_codec`] instead.
pub const ALLOWED: &[AllowedEncoder] = &[
    enc("nvd3d11h264enc", Codec::H264, F::NvencD3d11, V::Nvidia),
    enc("nvd3d11h265enc", Codec::H265, F::NvencD3d11, V::Nvidia),
    enc("nvd3d11av1enc", Codec::Av1, F::NvencD3d11, V::Nvidia),
    enc("nvh264enc", Codec::H264, F::NvencCuda, V::Nvidia),
    enc("nvh265enc", Codec::H265, F::NvencCuda, V::Nvidia),
    enc("nvav1enc", Codec::Av1, F::NvencCuda, V::Nvidia),
    enc("amfh264enc", Codec::H264, F::Amf, V::Amd),
    enc("amfh265enc", Codec::H265, F::Amf, V::Amd),
    enc("amfav1enc", Codec::Av1, F::Amf, V::Amd),
    enc("qsvh264enc", Codec::H264, F::Qsv, V::Intel),
    enc("qsvh265enc", Codec::H265, F::Qsv, V::Intel),
    enc("qsvav1enc", Codec::Av1, F::Qsv, V::Intel),
    enc("vah264enc", Codec::H264, F::VaApi, V::Os),
    enc("vah265enc", Codec::H265, F::VaApi, V::Os),
    enc("vaav1enc", Codec::Av1, F::VaApi, V::Os),
    AllowedEncoder {
        element: "openh264enc",
        codec: Codec::H264,
        family: F::OpenH264,
        vendor: V::Os,
        linux_only: true,
    },
];

/// Encoders that must never be selected or bundled (GPL, or software H.264/HEVC we may
/// not ship). Matched exactly or, for entries ending in `*`, by prefix.
pub const DENIED: &[&str] = &[
    "x264enc",
    "x265enc",
    "avenc_libx264",
    "avenc_libx264rgb",
    "avenc_libx265",
    "avenc_libopenh264",
    "avenc_h264*",
    "avenc_hevc*",
    "svthevcenc",
    "mpeg2enc",
];

/// GStreamer plugin files (without extension and `gst` prefix / `lib` prefix) that are
/// GPL and must never be bundled.
pub const DENIED_PLUGINS: &[&str] = &[
    "x264", "x265", "a52dec", "cdio", "dvdread", "mpeg2dec", "sid", "dtsdec", "mpeg2enc", "mplex",
    "resindvd", "faad",
];

/// True if `element` is on the denylist.
pub fn is_denied(element: &str) -> bool {
    DENIED.iter().any(|d| match d.strip_suffix('*') {
        Some(prefix) => element.starts_with(prefix),
        None => element == *d,
    })
}

/// If `element` is a Media Foundation encoder (`mf<codec>enc` or
/// `mf<codec>device<N>enc`), its codec.
pub fn media_foundation_codec(element: &str) -> Option<Codec> {
    let rest = element.strip_prefix("mf")?;
    let rest = rest.strip_suffix("enc")?;
    let (codec, tail) = if let Some(t) = rest.strip_prefix("h264") {
        (Codec::H264, t)
    } else if let Some(t) = rest.strip_prefix("h265") {
        (Codec::H265, t)
    } else {
        let t = rest.strip_prefix("av1")?;
        (Codec::Av1, t)
    };
    if tail.is_empty() {
        return Some(codec);
    }
    let digits = tail.strip_prefix("device")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(codec)
}

/// Look up an element on the allowlist (including Media Foundation names). Denied names
/// always return `None`.
pub fn allowed(element: &str) -> Option<AllowedEncoder> {
    if is_denied(element) {
        return None;
    }
    if let Some(a) = ALLOWED.iter().find(|a| a.element == element) {
        if a.linux_only && !cfg!(target_os = "linux") {
            return None;
        }
        return Some(*a);
    }
    if cfg!(windows) {
        let codec = media_foundation_codec(element)?;
        return Some(AllowedEncoder {
            element: "",
            codec,
            family: F::MediaFoundation,
            vendor: V::Os,
            linux_only: false,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_encoders_are_never_allowed() {
        for name in [
            "x264enc",
            "x265enc",
            "avenc_libx264",
            "avenc_h264_nvenc",
            "avenc_hevc_amf",
            "svthevcenc",
        ] {
            assert!(is_denied(name), "{name}");
            assert!(allowed(name).is_none(), "{name}");
        }
        for a in ALLOWED {
            assert!(
                !is_denied(a.element),
                "{} is both allowed and denied",
                a.element
            );
        }
    }

    #[test]
    fn media_foundation_names() {
        assert_eq!(media_foundation_codec("mfh264enc"), Some(Codec::H264));
        assert_eq!(
            media_foundation_codec("mfh265device2enc"),
            Some(Codec::H265)
        );
        assert_eq!(media_foundation_codec("mfav1device10enc"), Some(Codec::Av1));
        assert_eq!(media_foundation_codec("mfh264deviceenc"), None);
        assert_eq!(media_foundation_codec("mfaacenc"), None);
        assert_eq!(media_foundation_codec("mfh264dec"), None);
        assert_eq!(media_foundation_codec("mfh264device2xenc"), None);
    }

    #[test]
    fn openh264_only_on_linux() {
        assert_eq!(allowed("openh264enc").is_some(), cfg!(target_os = "linux"));
    }

    #[test]
    fn nvenc_is_allowed() {
        let a = allowed("nvd3d11h264enc").unwrap();
        assert_eq!((a.codec, a.vendor), (Codec::H264, EncoderVendor::Nvidia));
    }
}
