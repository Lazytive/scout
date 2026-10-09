//! Encoder discovery: which allowlisted encoders exist on this machine, in what order to
//! prefer them, and whether they actually work (a short dummy encode).
//!
//! The policy (allowlist/denylist) lives in [`policy`]. Ranking prefers the vendor of a
//! GPU that is present, D3D11-native NVENC over CUDA NVENC, hardware Media Foundation
//! encoders over Microsoft's software MFT.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

pub mod policy;

use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use klip_media::Codec;
use klip_platform::GpuVendor;

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
}

/// Outcome of a dummy encode.
#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub choice: EncoderChoice,
    pub ok: bool,
    pub error: Option<String>,
    pub elapsed: Duration,
}

/// Initialise GStreamer (idempotent).
pub fn init() -> Result<(), ProbeError> {
    gst::init().map_err(|e| ProbeError::Init(e.to_string()))
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

/// Allowlisted encoders for `codec` that GStreamer has registered, best first.
/// Requires [`init`].
pub fn candidates(codec: Codec, gpus: &[GpuVendor]) -> Vec<EncoderChoice> {
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
        let vendor_ok = vendor_present(a.vendor, gpus);
        let order = policy::ALLOWED
            .iter()
            .position(|x| x.element == name)
            .and_then(|p| u32::try_from(p).ok())
            .unwrap_or(50);
        // Vendor encoders for a GPU we have: 0..; OS hardware MFT: 100; NVENC/AMF/QSV
        // without a matching adapter: 200; software MFT / VA / openh264: 300.
        let rank = match a.family {
            EncoderFamily::MediaFoundation if mf_is_hardware(&description) => 100,
            EncoderFamily::MediaFoundation | EncoderFamily::OpenH264 => 300,
            EncoderFamily::VaApi => 50,
            _ if vendor_ok => order,
            _ => 200u32.saturating_add(order),
        };
        out.push(EncoderChoice {
            element: name,
            codec,
            family: a.family,
            vendor: a.vendor,
            description,
            rank,
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
pub fn probe_all(codec: Codec, gpus: &[GpuVendor], timeout: Duration) -> Vec<ProbeResult> {
    candidates(codec, gpus)
        .iter()
        .map(|c| {
            let r = probe(c, timeout);
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
    timeout: Duration,
) -> Result<EncoderChoice, ProbeError> {
    for c in candidates(codec, gpus) {
        let r = probe(&c, timeout);
        if r.ok {
            return Ok(c);
        }
        tracing::warn!(element = %c.element, error = ?r.error, "encoder probe failed");
    }
    Err(ProbeError::NoEncoder(codec.name()))
}
