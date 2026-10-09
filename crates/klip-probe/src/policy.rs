//! Which encoders Klip may ever use (AGENTS.md rules 5 and 6).
//!
//! The **allowlist** is the gate: only GPU vendor encoders, OS encoders (Media
//! Foundation, Direct3D 12 Video Encode) and, on Linux only, openh264 (meant to be Cisco's binary from the Flatpak
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
    /// Direct3D 12 Video Encode: the GPU's hardware encoder through the OS video API,
    /// for any vendor whose driver supports it (no vendor runtime needed).
    D3d12,
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
    enc("d3d12h264enc", Codec::H264, F::D3d12, V::Os),
    enc("d3d12h265enc", Codec::H265, F::D3d12, V::Os),
    enc("d3d12av1enc", Codec::Av1, F::D3d12, V::Os),
    enc("mfh264enc", Codec::H264, F::MediaFoundation, V::Os),
    enc("mfh265enc", Codec::H265, F::MediaFoundation, V::Os),
    enc("mfav1enc", Codec::Av1, F::MediaFoundation, V::Os),
    enc("vah264enc", Codec::H264, F::VaApi, V::Os),
    enc("vah265enc", Codec::H265, F::VaApi, V::Os),
    enc("vaav1enc", Codec::Av1, F::VaApi, V::Os),
    // Low-power (fixed-function) VA entry points; Intel Arc encodes AV1 only this way.
    enc("vah264lpenc", Codec::H264, F::VaApi, V::Os),
    enc("vah265lpenc", Codec::H265, F::VaApi, V::Os),
    enc("vaav1lpenc", Codec::Av1, F::VaApi, V::Os),
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

/// The element name without a per-device suffix: GStreamer registers the encoder of the
/// first adapter as e.g. `mfh264enc` and the others as `mfh264device<N>enc` (Media
/// Foundation, D3D12, NVENC D3D11, AMF, QSV all do this). `None` if `element` has no such
/// suffix. VA-API names them after the render node instead (see [`va_render_device`]).
pub fn without_device_suffix(element: &str) -> Option<String> {
    if let Some((base, _)) = va_render_device(element) {
        return Some(base);
    }
    let rest = element.strip_suffix("enc")?;
    let pos = rest.rfind("device")?;
    let (base, tail) = rest.split_at(pos);
    let digits = tail.strip_prefix("device")?;
    if base.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{base}enc"))
}

/// VA-API registers the encoders of every device after the first under the device's
/// render node: `varenderD129h264enc` is `vah264enc` on `/dev/dri/renderD129`. Returns
/// the base name and the node number.
pub fn va_render_device(element: &str) -> Option<(String, u32)> {
    let rest = element.strip_prefix("varender")?;
    let rest = rest.strip_prefix('D').or_else(|| rest.strip_prefix('d'))?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let node = rest.get(..digits)?.parse::<u32>().ok()?;
    let codec = rest.get(digits..)?;
    if !codec.ends_with("enc") || codec.len() <= 3 {
        return None;
    }
    Some((format!("va{codec}"), node))
}

/// Look up an element on the allowlist, including per-device names (see
/// [`without_device_suffix`]). The returned entry's `element` is the allowlisted base
/// name. Denied names always return `None`.
pub fn allowed(element: &str) -> Option<AllowedEncoder> {
    if is_denied(element) {
        return None;
    }
    let found = ALLOWED.iter().find(|a| a.element == element).or_else(|| {
        let base = without_device_suffix(element)?;
        ALLOWED.iter().find(|a| a.element == base)
    })?;
    if found.linux_only && !cfg!(target_os = "linux") {
        return None;
    }
    Some(*found)
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
    fn per_device_names() {
        let codec = |n: &str| allowed(n).map(|a| a.codec);
        assert_eq!(codec("mfh264enc"), Some(Codec::H264));
        assert_eq!(codec("mfh265device2enc"), Some(Codec::H265));
        assert_eq!(codec("mfav1device10enc"), Some(Codec::Av1));
        assert_eq!(codec("d3d12h264device1enc"), Some(Codec::H264));
        assert_eq!(codec("nvd3d11h265device3enc"), Some(Codec::H265));
        assert_eq!(codec("qsvav1device2enc"), Some(Codec::Av1));
        assert_eq!(codec("mfh264deviceenc"), None);
        assert_eq!(codec("mfaacenc"), None);
        assert_eq!(codec("mfh264dec"), None);
        assert_eq!(codec("mfh264device2xenc"), None);
        assert_eq!(codec("device1enc"), None);
        assert_eq!(codec("x264device1enc"), None);
        assert_eq!(
            allowed("d3d12h264device1enc").unwrap().family,
            EncoderFamily::D3d12
        );
        assert_eq!(
            without_device_suffix("mfh264device12enc").as_deref(),
            Some("mfh264enc")
        );
    }

    #[test]
    fn va_device_names_map_to_the_base_encoder() {
        assert_eq!(
            va_render_device("varenderD129h264enc"),
            Some(("vah264enc".to_string(), 129))
        );
        assert_eq!(
            allowed("varenderD130av1lpenc").map(|a| a.element),
            Some("vaav1lpenc")
        );
        assert_eq!(va_render_device("varenderDenc"), None);
        assert_eq!(va_render_device("varenderD129enc"), None);
        assert_eq!(va_render_device("vah264enc"), None);
        assert!(allowed("varenderD129x264enc").is_none());
    }

    #[test]
    fn denied_names_with_device_suffix_stay_denied() {
        // A denied software encoder never sneaks in under a device-style name.
        assert!(allowed("x264enc").is_none());
        assert!(allowed("avenc_h264_device1enc").is_none());
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
