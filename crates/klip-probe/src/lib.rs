//! Encoder discovery: which allowlisted encoders exist on this machine, in what order to
//! prefer them, and whether they actually work (a short dummy encode).
//!
//! The policy (allowlist/denylist) lives in [`policy`]. Ranking prefers encoders on the
//! GPU that shows the captured picture (hybrid laptops: no copy between GPUs), then the
//! vendor SDK encoders (NVENC D3D11, AMF, QSV), hardware Media Foundation MFTs, D3D12
//! Video Encode, CUDA NVENC, and Microsoft's software MFT last. [`cache`] remembers which
//! encoders worked, so later starts don't probe again.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

pub mod cache;
pub mod isolated;
pub mod policy;

use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use klip_media::Codec;
use klip_platform::GpuVendor;

pub use cache::{CachedResult, ProbeCache, fingerprint};
pub use isolated::{Prober, WORKER_FLAG, worker_main};
pub use policy::{EncoderFamily, EncoderVendor, allowed, is_denied};

/// Errors from initialising GStreamer or selecting an encoder.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("GStreamer could not be initialised: {0}")]
    Init(String),
    #[error("no working {0} encoder found")]
    NoEncoder(&'static str),
}

/// An allowlisted encoder that is installed on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderChoice {
    pub element: String,
    pub codec: Codec,
    pub family: EncoderFamily,
    pub vendor: EncoderVendor,
    /// GStreamer's long name, e.g. "NVENC H.264 Video Encoder Direct3D11 Mode".
    pub description: String,
    /// Lower is preferred.
    pub rank: u32,
    /// Vendor of the GPU the element encodes on (`None`: software or unknown).
    pub gpu: Option<GpuVendor>,
    /// 0 for the element of the first adapter GStreamer registered, `N` for
    /// `...device<N>enc`.
    pub device_index: u32,
}

impl EncoderChoice {
    /// Whether frames must leave the GPU on the way in (CUDA upload, software encoders).
    pub fn needs_system_memory(&self) -> bool {
        match self.family {
            EncoderFamily::NvencCuda | EncoderFamily::VaApi | EncoderFamily::OpenH264 => true,
            EncoderFamily::MediaFoundation => !mf_is_hardware(&self.description),
            EncoderFamily::NvencD3d11
            | EncoderFamily::Amf
            | EncoderFamily::Qsv
            | EncoderFamily::D3d12 => false,
        }
    }
}

/// Outcome of a dummy encode.
#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub choice: EncoderChoice,
    pub ok: bool,
    pub error: Option<String>,
    pub elapsed: Duration,
}

/// Initialise GStreamer (idempotent). On Windows this also starts Media Foundation
/// before anything can open an NVENC session; see [`warm_up_media_foundation`].
pub fn init() -> Result<(), ProbeError> {
    gst::init().map_err(|e| ProbeError::Init(e.to_string()))?;
    static WARM_UP: std::sync::Once = std::sync::Once::new();
    WARM_UP.call_once(warm_up_media_foundation);
    Ok(())
}

/// Create (and drop) one Media Foundation element so GStreamer's MF plugin starts Media
/// Foundation now. Measured on the dev machine (GStreamer 1.28.6, NVIDIA driver
/// 32.0.16.1742): if Media Foundation starts *after* a GStreamer NVENC session existed in
/// the process (e.g. `mfaacenc` for audio starting after the video pipeline), every later
/// NVENC session in that process fails with `NV_ENC_ERR_INVALID_VERSION`; started first,
/// NVENC keeps working (regression test `nvenc_survives_media_foundation_in_between`,
/// ADR 0009). A no-op where the MF plugin is absent.
fn warm_up_media_foundation() {
    if !cfg!(windows) {
        return;
    }
    for name in ["mfaacenc", "mfh264enc"] {
        if let Ok(e) = gst::ElementFactory::make(name).build() {
            drop(e);
            tracing::debug!(element = name, "Media Foundation started");
            return;
        }
    }
}

fn vendor_present(v: EncoderVendor, gpus: &[GpuVendor]) -> bool {
    let want = match v {
        EncoderVendor::Nvidia => GpuVendor::Nvidia,
        EncoderVendor::Amd => GpuVendor::Amd,
        EncoderVendor::Intel => GpuVendor::Intel,
        EncoderVendor::Os => return true,
    };
    gpus.contains(&want)
}

fn mf_is_hardware(long_name: &str) -> bool {
    let l = long_name.to_ascii_lowercase();
    ["nvidia", "amd", "intel", "quick sync", "qualcomm"]
        .iter()
        .any(|v| l.contains(v))
}

/// Names of every installed, allowlisted video encoder element (all codecs), for the
/// cache [`fingerprint`]. Requires [`init`].
pub fn installed_encoders() -> Vec<String> {
    let mut v: Vec<String> = gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::VIDEO_ENCODER,
        gst::Rank::NONE,
    )
    .into_iter()
    .map(|f| f.name().to_string())
    .filter(|n| allowed(n).is_some())
    .collect();
    v.sort();
    v
}

/// The GPU vendor an encoder runs on, from its family or (Media Foundation, D3D12) its
/// long name. Read without creating the element: creating and dropping encoder elements
/// before an NVENC session opens made NVENC fail in the same process (session 3,
/// GStreamer 1.28.6, driver 32.0.16.1742; see ADR 0009).
fn gpu_of(family: EncoderFamily, vendor: EncoderVendor, long_name: &str) -> Option<GpuVendor> {
    match vendor {
        EncoderVendor::Nvidia => return Some(GpuVendor::Nvidia),
        EncoderVendor::Amd => return Some(GpuVendor::Amd),
        EncoderVendor::Intel => return Some(GpuVendor::Intel),
        EncoderVendor::Os => {}
    }
    if !matches!(
        family,
        EncoderFamily::MediaFoundation | EncoderFamily::D3d12
    ) {
        return None;
    }
    let l = long_name.to_ascii_lowercase();
    if ["nvidia", "geforce", "quadro"]
        .iter()
        .any(|v| l.contains(v))
    {
        Some(GpuVendor::Nvidia)
    } else if ["amd", "radeon"].iter().any(|v| l.contains(v)) {
        Some(GpuVendor::Amd)
    } else if ["intel", "quick sync"].iter().any(|v| l.contains(v)) {
        Some(GpuVendor::Intel)
    } else {
        None
    }
}

/// `N` for a per-device element `...device<N>enc`, 0 for the base name (GStreamer gives
/// the base name to the first adapter it registers).
fn device_index(element: &str) -> u32 {
    policy::without_device_suffix(element)
        .and_then(|_| {
            let rest = element.strip_suffix("enc")?;
            let pos = rest.rfind("device")?;
            rest.get(pos.checked_add(6)?..)?.parse::<u32>().ok()
        })
        .unwrap_or(0)
}

/// Rank of an encoder (lower is better). `prefer` is the vendor of the GPU that shows
/// the captured picture; encoders there avoid a copy between GPUs (hybrid laptops).
fn rank_of(
    family: EncoderFamily,
    vendor_ok: bool,
    hardware_mf: bool,
    order: u32,
    gpu: Option<GpuVendor>,
    device: u32,
    prefer: Option<GpuVendor>,
) -> u32 {
    let family_rank = match family {
        EncoderFamily::NvencD3d11 | EncoderFamily::Amf | EncoderFamily::Qsv if vendor_ok => order,
        EncoderFamily::VaApi => 30,
        // D3D12 before hardware MFTs: a session on NVIDIA's H.264 MFT makes later NVENC
        // sessions in the process fail (ADR 0009); D3D12 doesn't.
        EncoderFamily::D3d12 => 40,
        EncoderFamily::MediaFoundation if hardware_mf => 60,
        EncoderFamily::NvencCuda if vendor_ok => 80,
        EncoderFamily::NvencD3d11
        | EncoderFamily::Amf
        | EncoderFamily::Qsv
        | EncoderFamily::NvencCuda => 500u32.saturating_add(order),
        // Software: always last, wherever it runs.
        EncoderFamily::MediaFoundation | EncoderFamily::OpenH264 => return 9000,
    };
    let adapter_rank: u32 = match (gpu, prefer) {
        (Some(g), Some(p)) if g == p => 0,
        (Some(_), Some(_)) => 2000,
        _ => 1000,
    };
    adapter_rank
        .saturating_add(family_rank)
        .saturating_add(device.min(9))
}

/// Allowlisted encoders for `codec` that GStreamer has registered, best first (see the
/// crate docs for the order). `gpus`: vendors of the hardware adapters present;
/// `prefer`: vendor of the GPU that drives the captured monitor, if known. Requires
/// [`init`].
pub fn candidates(
    codec: Codec,
    gpus: &[GpuVendor],
    prefer: Option<GpuVendor>,
) -> Vec<EncoderChoice> {
    let mut out: Vec<EncoderChoice> = Vec::new();
    let factories = gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::VIDEO_ENCODER,
        gst::Rank::NONE,
    );
    for f in factories {
        let name = f.name().to_string();
        let Some(a) = allowed(&name) else {
            continue;
        };
        if a.codec != codec {
            continue;
        }
        let description = f.longname().to_string();
        let order = policy::ALLOWED
            .iter()
            .position(|x| x.element == a.element)
            .and_then(|p| u32::try_from(p).ok())
            .unwrap_or(50);
        let gpu = gpu_of(a.family, a.vendor, &description);
        let device = device_index(&name);
        let rank = rank_of(
            a.family,
            vendor_present(a.vendor, gpus),
            mf_is_hardware(&description),
            order,
            gpu,
            device,
            prefer,
        );
        out.push(EncoderChoice {
            element: name,
            codec,
            family: a.family,
            vendor: a.vendor,
            description,
            rank,
            gpu,
            device_index: device,
        });
    }
    out.sort_by(|a, b| (a.rank, &a.element).cmp(&(b.rank, &b.element)));
    out
}

fn parser_for(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264parse",
        Codec::H265 => "h265parse",
        Codec::Av1 => "av1parse",
        Codec::Aac => "aacparse",
        Codec::Opus => "opusparse",
    }
}

/// Elements between a system-memory NV12 source and the encoder.
fn upload_for(family: EncoderFamily) -> &'static str {
    match family {
        EncoderFamily::NvencD3d11 | EncoderFamily::Amf | EncoderFamily::Qsv => "d3d11upload ! ",
        EncoderFamily::D3d12 => "d3d12upload ! ",
        EncoderFamily::NvencCuda => "cudaupload ! ",
        EncoderFamily::MediaFoundation | EncoderFamily::VaApi | EncoderFamily::OpenH264 => "",
    }
}

/// Encode one second of 1280x720@60 test video with `choice` and report whether it
/// worked. Never panics; failures are in the result.
pub fn probe(choice: &EncoderChoice, timeout: Duration) -> ProbeResult {
    let start = Instant::now();
    let res = run_probe(choice, timeout);
    ProbeResult {
        choice: choice.clone(),
        ok: res.is_ok(),
        error: res.err(),
        elapsed: start.elapsed(),
    }
}

fn run_probe(choice: &EncoderChoice, timeout: Duration) -> Result<(), String> {
    // The element name comes from GStreamer's registry and passed the allowlist, so it is
    // a plain identifier; refuse anything else before building a launch string.
    if !choice
        .element
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("invalid element name".into());
    }
    let desc = format!(
        "videotestsrc num-buffers=60 pattern=ball ! \
         video/x-raw,format=NV12,width=1280,height=720,framerate=60/1 ! {}{} ! {} ! fakesink sync=false",
        upload_for(choice.family),
        choice.element,
        parser_for(choice.codec),
    );
    let pipeline = gst::parse::launch(&desc).map_err(|e| e.to_string())?;
    let bus = pipeline.bus().ok_or("pipeline has no bus")?;
    let result = (|| {
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("could not start: {e}"))?;
        let deadline = Instant::now().checked_add(timeout).ok_or("bad timeout")?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("timed out".to_string());
            }
            let msg = bus.timed_pop_filtered(
                gst::ClockTime::from_nseconds(u64::try_from(left.as_nanos()).unwrap_or(u64::MAX)),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
            match msg.as_ref().map(|m| m.view()) {
                Some(gst::MessageView::Eos(_)) => return Ok(()),
                Some(gst::MessageView::Error(e)) => return Err(e.error().to_string()),
                Some(_) => continue,
                None => return Err("timed out".to_string()),
            }
        }
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Probe every candidate for `codec`; returns them best first with results.
pub fn probe_all(
    codec: Codec,
    gpus: &[GpuVendor],
    prefer: Option<GpuVendor>,
    prober: &Prober,
    timeout: Duration,
) -> Vec<ProbeResult> {
    candidates(codec, gpus, prefer)
        .iter()
        .map(|c| {
            let r = prober.probe(c, timeout);
            tracing::info!(
                element = %c.element,
                ok = r.ok,
                ms = u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX),
                "encoder probe"
            );
            r
        })
        .collect()
}

/// The best working encoder for `codec`, probing candidates in order until one works.
pub fn best(
    codec: Codec,
    gpus: &[GpuVendor],
    prefer: Option<GpuVendor>,
    timeout: Duration,
) -> Result<EncoderChoice, ProbeError> {
    let mut cache = ProbeCache::default();
    ranked(codec, gpus, prefer, &mut cache, &Prober::InProcess, timeout)
        .into_iter()
        .next()
        .ok_or(ProbeError::NoEncoder(codec.name()))
}

/// Encoders to try for `codec`, best first: the first one is known to work (probed now or
/// earlier, per `cache`); the rest are fallbacks that are not known to fail (untested
/// ones are tried when needed). Encoders that failed a probe, or failed to start
/// [`cache::RUNTIME_FAILURE_LIMIT`] times, are left out. New probe results go into
/// `cache` (the caller saves it). Empty if nothing works.
pub fn ranked(
    codec: Codec,
    gpus: &[GpuVendor],
    prefer: Option<GpuVendor>,
    cache: &mut ProbeCache,
    prober: &Prober,
    timeout: Duration,
) -> Vec<EncoderChoice> {
    let mut out: Vec<EncoderChoice> = Vec::new();
    let mut found = false;
    for c in candidates(codec, gpus, prefer) {
        match cache.get(&c.element) {
            Some(r) if !r.usable() => continue,
            Some(r) if r.ok => {
                // Known good: becomes the first working one if none was found yet.
                if !found {
                    found = true;
                    out.push(c);
                } else {
                    out.push(c);
                }
                continue;
            }
            _ => {}
        }
        if found {
            out.push(c);
            continue;
        }
        let r = prober.probe(&c, timeout);
        tracing::info!(
            element = %c.element,
            ok = r.ok,
            ms = u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX),
            "encoder probe"
        );
        cache.record(&r);
        if r.ok {
            found = true;
            out.push(c);
        } else {
            tracing::warn!(element = %c.element, error = ?r.error, "encoder probe failed");
        }
    }
    if found { out } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_prefers_the_capture_adapter_then_family() {
        // Hybrid laptop: the monitor hangs off the Intel iGPU.
        let here = Some(GpuVendor::Intel);
        let intel = Some(GpuVendor::Intel);
        let nv = Some(GpuVendor::Nvidia);
        let r = |f, vendor_ok, hw_mf, order, gpu, dev| {
            rank_of(f, vendor_ok, hw_mf, order, gpu, dev, here)
        };
        let qsv_here = r(EncoderFamily::Qsv, true, false, 9, intel, 0);
        let d3d12_here = r(EncoderFamily::D3d12, true, false, 0, intel, 0);
        let mf_here = r(EncoderFamily::MediaFoundation, true, true, 0, intel, 0);
        let mf_here_dev1 = r(EncoderFamily::MediaFoundation, true, true, 0, intel, 1);
        let cuda_unknown = r(EncoderFamily::NvencCuda, true, false, 3, None, 0);
        let nvenc_other = r(EncoderFamily::NvencD3d11, true, false, 0, nv, 0);
        let amf_no_gpu = r(EncoderFamily::Amf, false, false, 6, Some(GpuVendor::Amd), 0);
        let software = r(EncoderFamily::MediaFoundation, true, false, 0, None, 0);
        let order = [
            qsv_here,
            d3d12_here,
            mf_here,
            mf_here_dev1,
            cuda_unknown,
            nvenc_other,
            amf_no_gpu,
            software,
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        // Without a known capture GPU, the family order decides.
        let n = rank_of(EncoderFamily::NvencD3d11, true, false, 0, nv, 0, None);
        let m = rank_of(
            EncoderFamily::MediaFoundation,
            true,
            true,
            0,
            intel,
            0,
            None,
        );
        assert!(n < m);
    }

    #[test]
    fn gpu_and_device_from_names() {
        use EncoderFamily as F;
        use EncoderVendor as V;
        assert_eq!(gpu_of(F::Qsv, V::Intel, ""), Some(GpuVendor::Intel));
        assert_eq!(
            gpu_of(
                F::MediaFoundation,
                V::Os,
                "Media Foundation NVIDIA H.264 Encoder MFT"
            ),
            Some(GpuVendor::Nvidia)
        );
        assert_eq!(
            gpu_of(F::MediaFoundation, V::Os, "Media Foundation AMDh264Encoder"),
            Some(GpuVendor::Amd)
        );
        assert_eq!(
            gpu_of(
                F::D3d12,
                V::Os,
                "Direct3D12 H.264 Intel(R) Arc(TM) A770 Graphics Encoder"
            ),
            Some(GpuVendor::Intel)
        );
        assert_eq!(
            gpu_of(
                F::MediaFoundation,
                V::Os,
                "Media Foundation H264 Encoder MFT"
            ),
            None
        );
        assert_eq!(device_index("mfh264enc"), 0);
        assert_eq!(device_index("mfh264device2enc"), 2);
        assert_eq!(device_index("d3d12h264device11enc"), 11);
        assert_eq!(device_index("nvd3d11h264enc"), 0);
    }

    #[test]
    #[ignore = "hardware: needs GStreamer and a GPU encoder; run via `cargo xtask e2e`"]
    fn ranked_list_prefers_monitor_adapter_and_uses_the_cache() {
        init().unwrap();
        let gpus = klip_platform::list_gpus().unwrap();
        let vendors: Vec<_> = gpus
            .iter()
            .filter(|g| !g.software)
            .map(|g| g.vendor)
            .collect();
        let monitor_gpu = klip_platform::list_monitors()
            .unwrap()
            .into_iter()
            .find(|m| m.primary)
            .and_then(|m| m.adapter_luid)
            .and_then(|l| gpus.iter().find(|g| g.luid == l))
            .map(|g| g.vendor);
        let list = candidates(Codec::H264, &vendors, monitor_gpu);
        for c in &list {
            println!("{:>5} {:<24} {:?}", c.rank, c.element, c.gpu);
        }
        // Hardware encoders on the monitor's GPU come before all others.
        if let Some(v) = monitor_gpu
            && let Some(last_here) = list.iter().rposition(|c| c.gpu == Some(v))
        {
            let first_elsewhere = list
                .iter()
                .position(|c| c.gpu.is_some_and(|g| g != v))
                .unwrap_or(usize::MAX);
            assert!(last_here < first_elsewhere);
        }
        let mut cache = ProbeCache::new("test");
        let t = Instant::now();
        let first = ranked(
            Codec::H264,
            &vendors,
            monitor_gpu,
            &mut cache,
            &Prober::InProcess,
            Duration::from_secs(10),
        );
        println!("first ranking with probes: {} ms", t.elapsed().as_millis());
        assert!(
            !first.is_empty(),
            "this machine has a working H.264 encoder"
        );
        assert!(cache.get(&first[0].element).unwrap().ok);
        let t = Instant::now();
        let again = ranked(
            Codec::H264,
            &vendors,
            monitor_gpu,
            &mut cache,
            &Prober::InProcess,
            Duration::from_secs(10),
        );
        let ms = t.elapsed().as_millis();
        println!("cached ranking: {ms} ms");
        assert_eq!(first, again);
        assert!(ms < 500, "cached ranking must not probe ({ms} ms)");
    }
}
