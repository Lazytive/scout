//! Audio capture: WASAPI (own reader, QPC timestamps) → 48 kHz stereo → AAC-LC (Media
//! Foundation) → packets.
//!
//! Each input is a `klip_platform::AudioCapture` thread pushing into an `appsrc`. Packet
//! timestamps come from WASAPI's QueryPerformanceCounter capture times (the clock behind
//! WGC frame times and GStreamer's system clock), made continuous by
//! [`klip_media::DriftAligner`]. One pipeline holds every input of a recording:
//!
//! ```text
//! appsrc (F32 48 kHz stereo, QPC-stamped) → audiorate ─┐
//!   one input:   → AAC chain → appsink (track N)                              │
//!   several:     → tee ─→ queue → audiomixer → AAC chain → appsink ("Mix", N) │
//!                      └→ queue → AAC chain → appsink (N+1+i, if separate)    │
//! AAC chain: audioconvert → S16LE → queue → AAC encoder → aacparse → raw AAC
//! (`mfaacenc` on Windows; `fdkaacenc`, else `avenc_aac` on Linux; ADR 0005)
//! ```
//!
//! Per-process loopback stamps its packets 13–19 ms late (a different amount per session),
//! so while a process is recorded the system mix is captured as a timing reference and
//! the lag is measured continuously (`lag.rs`).
//!
//! The mix comes first because players (browsers, Discord, phones) only play the first
//! audio track; the separate tracks are for editors. A device that fails or is unplugged
//! records silence and posts a warning (and is retried) instead of stopping the
//! recording. Audio runs in its own pipeline (sharing the video's
//! [`TimeBase`](crate::TimeBase)) so it can never take video down with it.
//!
//! GStreamer's `wasapi2src` was used first and dropped: it stamps buffers on arrival, which
//! left audio −20 ms (system loopback) to +16 ms (process loopback) off, varying per run
//! (`tests/timestamp_lateness.rs`, ADR 0008).

use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use klip_media::{DriftAligner, Nanos};
use klip_platform::{AudioCapture, AudioCaptureSink, AudioPacket, CaptureEndpoint};

use crate::lag::LagEstimator;
use crate::{
    CaptureError, CaptureSession, CaptureStats, PacketSink, TimeBase, Timing, attach_sink_with,
    gst_err, make, props,
};

type SharedLag = Arc<Mutex<LagEstimator>>;

fn mono(p: &AudioPacket<'_>) -> Vec<f32> {
    let ch = CHANNELS as usize;
    match p.data {
        Some(d) => d
            .chunks(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect(),
        None => vec![0.0; usize::try_from(p.frames).unwrap_or(0)],
    }
}

/// Feeds the system mix to every process input's lag estimator.
struct Reference {
    estimators: Vec<SharedLag>,
}

impl AudioCaptureSink for Reference {
    fn packet(&mut self, p: AudioPacket<'_>) {
        let m = mono(&p);
        for e in &self.estimators {
            e.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_reference(p.qpc_ns, &m);
        }
    }

    fn notice(&mut self, message: String) {
        tracing::debug!(%message, "timing reference capture");
    }
}

/// Sample rate of every audio track.
pub const SAMPLE_RATE: u32 = 48_000;
/// Channels of every audio track.
pub const CHANNELS: u32 = 2;
/// Default AAC bitrate per track (ADR 0005).
pub const DEFAULT_BITRATE: u32 = 160_000;
/// Bitrates `mfaacenc` accepts for 48 kHz stereo.
pub const BITRATES: [u32; 6] = [96_000, 128_000, 160_000, 192_000, 256_000, 320_000];

/// AAC encoders in order of preference: the OS's (Media Foundation) on Windows; on Linux
/// FDK AAC (in Flatpak runtimes and Fedora), else FFmpeg's native encoder (LGPL).
#[cfg(windows)]
const AAC_ENCODERS: &[&str] = &["mfaacenc"];
#[cfg(not(windows))]
const AAC_ENCODERS: &[&str] = &["fdkaacenc", "avenc_aac"];

/// The AAC encoder element this machine uses (requires GStreamer to be initialised).
/// `KLIP_AAC_ENCODER` picks another one from the list (measurements).
pub fn aac_encoder() -> Option<&'static str> {
    let wanted = std::env::var("KLIP_AAC_ENCODER").ok();
    AAC_ENCODERS
        .iter()
        .copied()
        .filter(|e| wanted.as_deref().is_none_or(|w| w == *e))
        .find(|e| gst::ElementFactory::find(e).is_some())
}

/// How much later than its input timestamps `encoder`'s decoded output plays; packets are
/// shifted back by this. Measured with `klip-recorder/tests/synthetic_audio.rs` (beeps
/// at known times → encode → MP4 → ffmpeg decode): Media Foundation already aligns its
/// output (beeps within 0.01 ms, so 0); FDK AAC (GStreamer 1.28.7, Fedora 44) plays
/// exactly 2048 samples late (42.667 ms at 48 kHz, all beeps), FFmpeg's `avenc_aac`
/// 1024 samples (21.333 ms).
pub fn aac_encoder_delay(encoder: &str) -> Nanos {
    match encoder {
        "fdkaacenc" => 42_666_667,
        "avenc_aac" => 21_333_333,
        _ => 0,
    }
}

/// Latency an `appsrc` input reports: packets reach it a few ms after capture, generated
/// silence up to ~60 ms after (see `klip_platform` WASAPI capture). The mixer waits this
/// long for every input.
const INPUT_LATENCY_NS: u64 = 80_000_000;

/// Where an input's audio comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioSource {
    /// Everything an output device plays. `None`: the default output.
    System { device: Option<String> },
    /// Everything except one process and its children (Windows 10 2004+).
    SystemExcept { pid: u32 },
    /// Only one process and its children (Windows 10 2004+).
    Process { pid: u32 },
    /// Several process trees, mixed (Windows 10 2004+). See [`crate::apps`].
    Apps { pids: Vec<u32> },
    /// A capture device such as a microphone. `None`: the default input.
    Device { device: Option<String> },
}

impl AudioSource {
    /// Short description for logs and status (no device names: they can be personal).
    pub fn describe(&self) -> String {
        match self {
            AudioSource::System { device: None } => "system audio (default output)".into(),
            AudioSource::System { device: Some(_) } => "system audio (chosen output)".into(),
            AudioSource::SystemExcept { pid } => format!("system audio except pid {pid}"),
            AudioSource::Process { pid } => format!("audio of pid {pid}"),
            AudioSource::Apps { pids } => format!("audio of pids {pids:?}"),
            AudioSource::Device { device: None } => "default input device".into(),
            AudioSource::Device { device: Some(_) } => "chosen input device".into(),
        }
    }

    /// The WASAPI captures this source needs (several for `Apps`).
    fn endpoints(&self) -> Vec<CaptureEndpoint> {
        match self {
            AudioSource::System { device } => vec![CaptureEndpoint::Loopback {
                device: device.clone(),
            }],
            AudioSource::SystemExcept { pid } => vec![CaptureEndpoint::Process {
                pid: *pid,
                include: false,
            }],
            AudioSource::Process { pid } => vec![CaptureEndpoint::Process {
                pid: *pid,
                include: true,
            }],
            AudioSource::Apps { pids } => pids
                .iter()
                .map(|pid| CaptureEndpoint::Process {
                    pid: *pid,
                    include: true,
                })
                .collect(),
            AudioSource::Device { device } => vec![CaptureEndpoint::Input {
                device: device.clone(),
            }],
        }
    }
}

/// One audio input of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioInput {
    /// Track name in the MP4 (e.g. "Game", "Microphone").
    pub label: String,
    pub source: AudioSource,
}

/// Audio settings of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioConfig {
    pub inputs: Vec<AudioInput>,
    /// AAC bitrate per track, one of [`BITRATES`].
    pub bitrate: u32,
    /// With several inputs: also keep every input on its own track after the mix.
    pub separate_tracks: bool,
}

/// Label of the mixed track.
pub const MIX_LABEL: &str = "Mix";

/// Most inputs one recording accepts, and most apps in one `Apps` input.
pub const MAX_INPUTS: usize = 8;

impl AudioConfig {
    /// Names of the tracks this config produces, in track order.
    pub fn track_labels(&self) -> Vec<String> {
        match self.inputs.as_slice() {
            [] => Vec::new(),
            [one] => vec![one.label.clone()],
            many => {
                let mut v = vec![MIX_LABEL.to_string()];
                if self.separate_tracks {
                    v.extend(many.iter().map(|i| i.label.clone()));
                }
                v
            }
        }
    }

    fn validate(&self) -> Result<(), CaptureError> {
        if self.inputs.is_empty() || self.inputs.len() > MAX_INPUTS {
            return Err(CaptureError::Config(format!(
                "1 to {MAX_INPUTS} audio inputs are supported"
            )));
        }
        if self
            .inputs
            .iter()
            .any(|i| i.source.endpoints().is_empty() || i.source.endpoints().len() > MAX_INPUTS)
        {
            return Err(CaptureError::Config(format!(
                "an app list needs 1 to {MAX_INPUTS} apps"
            )));
        }
        if !BITRATES.contains(&self.bitrate) {
            return Err(CaptureError::Config(format!(
                "audio bitrate must be one of {BITRATES:?}"
            )));
        }
        Ok(())
    }
}

fn capsfilter(caps: &str) -> Result<gst::Element, CaptureError> {
    crate::capsfilter(caps)
}

fn raw_caps() -> String {
    format!("audio/x-raw,format=F32LE,layout=interleaved,rate={SAMPLE_RATE},channels={CHANNELS}")
}

fn queue() -> Result<gst::Element, CaptureError> {
    let q = make("queue")?;
    // Up to 1 s of audio. Not leaky: encoding is far faster than real time, and a leaky
    // queue drops most of a burst (e.g. a test feeding audio faster than real time).
    props::set_str(&q, "max-size-buffers", "0");
    props::set_str(&q, "max-size-bytes", "0");
    props::set_str(&q, "max-size-time", "1000000000");
    Ok(q)
}

/// Build `audioconvert → S16LE 48 kHz stereo → queue → AAC encoder ([`aac_encoder`]) →
/// aacparse → raw AAC → appsink` in `bin`. Returns the first element (link raw audio to
/// it) and the appsink.
pub fn build_aac_chain(
    bin: &gst::Pipeline,
    bitrate: u32,
) -> Result<(gst::Element, gst_app::AppSink), CaptureError> {
    if !BITRATES.contains(&bitrate) {
        return Err(CaptureError::Config(format!(
            "audio bitrate must be one of {BITRATES:?}"
        )));
    }
    let name =
        aac_encoder().ok_or_else(|| CaptureError::MissingElement("an AAC encoder".into()))?;
    let convert = make("audioconvert")?;
    // Media Foundation and FDK take S16; FFmpeg's encoder takes float.
    let format = match name {
        "avenc_aac" => "format=F32LE,layout=interleaved",
        _ => "format=S16LE,layout=interleaved",
    };
    let raw = capsfilter(&format!(
        "audio/x-raw,{format},rate={SAMPLE_RATE},channels={CHANNELS}"
    ))?;
    let encoder = make(name)?;
    props::require(&encoder, "bitrate", &bitrate.to_string()).map_err(CaptureError::Config)?;
    let parser = make("aacparse")?;
    let out = capsfilter("audio/mpeg,mpegversion=4,stream-format=raw")?;
    let appsink = gst_app::AppSink::builder()
        .sync(false)
        .enable_last_sample(false)
        .max_buffers(0)
        .build();
    let chain = [
        convert.clone(),
        raw,
        queue()?,
        encoder,
        parser,
        out,
        appsink.clone().upcast(),
    ];
    bin.add_many(&chain).map_err(gst_err)?;
    gst::Element::link_many(&chain).map_err(gst_err)?;
    Ok((convert, appsink))
}

/// Offset from QueryPerformanceCounter ns to running time on `tb` (GStreamer's system
/// clock counts QPC on Windows, so this is constant; read both clocks back to back and
/// keep the tightest pair).
fn qpc_offset(tb: &TimeBase) -> Nanos {
    let mut best: Option<(Nanos, Nanos)> = None;
    for _ in 0..5 {
        let a = tb.now();
        let q = klip_platform::qpc_now_ns();
        let b = tb.now();
        let spread = b.saturating_sub(a);
        let mid = a.saturating_add(spread.checked_div(2).unwrap_or(0));
        if best.is_none_or(|(s, _)| spread < s) {
            best = Some((spread, mid.saturating_sub(q)));
        }
    }
    best.map_or(0, |(_, off)| off)
}

/// Turns captured packets into continuous, QPC-timed buffers on an `appsrc`.
struct Pusher {
    appsrc: gst_app::AppSrc,
    aligner: DriftAligner,
    /// QPC ns → running time.
    offset: Nanos,
    label: String,
    sink: Arc<dyn PacketSink>,
    /// For process loopback: how late its stamps are.
    lag: Option<SharedLag>,
}

impl AudioCaptureSink for Pusher {
    fn packet(&mut self, p: AudioPacket<'_>) {
        let late = match &self.lag {
            Some(l) => {
                let mut l = l.lock().unwrap_or_else(PoisonError::into_inner);
                l.push_delayed(p.qpc_ns, &mono(&p));
                l.lag()
            }
            None => 0,
        };
        let running = p.qpc_ns.saturating_sub(late).saturating_add(self.offset);
        let plan = self.aligner.align(running, p.frames);
        let ch = CHANNELS as usize;
        let given = usize::try_from(p.frames).unwrap_or(0);
        let frames = match plan.adjust {
            1 => given.saturating_add(1),
            -1 => given.saturating_sub(1),
            _ => given,
        };
        if frames == 0 || plan.pts < 0 {
            return;
        }
        let mut bytes: Vec<u8> = Vec::with_capacity(frames.saturating_mul(ch).saturating_mul(4));
        if let Some(d) = p.data {
            let take = given.min(frames).saturating_mul(ch);
            for s in d.iter().take(take) {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            // Repeat the last frame to add one.
            if frames > given
                && let Some(last) = d.get(take.saturating_sub(ch)..take)
            {
                for s in last {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
        bytes.resize(frames.saturating_mul(ch).saturating_mul(4), 0);
        let duration = u64::try_from(frames)
            .unwrap_or(0)
            .saturating_mul(1_000_000_000)
            .checked_div(u64::from(SAMPLE_RATE))
            .unwrap_or(0);
        let mut buffer = gst::Buffer::from_mut_slice(bytes);
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(
                u64::try_from(plan.pts).unwrap_or(0),
            ));
            b.set_duration(gst::ClockTime::from_nseconds(duration));
            if plan.discont {
                b.set_flags(gst::BufferFlags::DISCONT);
            }
        }
        // Fails only while the pipeline shuts down.
        let _ = self.appsrc.push_buffer(buffer);
    }

    fn notice(&mut self, message: String) {
        self.sink.on_warning(format!("{}: {message}", self.label));
    }
}

/// A capture that starts once its pipeline runs.
struct Pending {
    endpoint: CaptureEndpoint,
    appsrc: gst_app::AppSrc,
    label: String,
}

fn make_appsrc(name: &str) -> Result<gst_app::AppSrc, CaptureError> {
    let caps = gst::Caps::from_str(&raw_caps()).map_err(gst_err)?;
    let src = gst_app::AppSrc::builder()
        .name(name)
        .caps(&caps)
        .format(gst::Format::Time)
        .is_live(true)
        .do_timestamp(false)
        .block(false)
        // One second of audio; older data is dropped if the pipeline stalls.
        .max_bytes(
            u64::from(SAMPLE_RATE)
                .saturating_mul(u64::from(CHANNELS))
                .saturating_mul(4),
        )
        .min_latency(i64::try_from(INPUT_LATENCY_NS).unwrap_or(0))
        .build();
    props::set_str(src.upcast_ref(), "leaky-type", "downstream");
    Ok(src)
}

/// The source element of one input: an `appsrc`, or for several apps a bin mixing one
/// `appsrc` per app. Records the captures to start in `pending`.
fn make_input_source(
    source: &AudioSource,
    name: &str,
    label: &str,
    pending: &mut Vec<Pending>,
) -> Result<gst::Element, CaptureError> {
    let endpoints = source.endpoints();
    if let [endpoint] = endpoints.as_slice() {
        let appsrc = make_appsrc(name)?;
        pending.push(Pending {
            endpoint: endpoint.clone(),
            appsrc: appsrc.clone(),
            label: label.to_string(),
        });
        return Ok(appsrc.upcast());
    }
    let bin = gst::Bin::with_name(name);
    let mixer = make("audiomixer")?;
    props::set_str(&mixer, "start-time-selection", "first");
    props::set_str(&mixer, "ignore-inactive-pads", "true");
    bin.add(&mixer).map_err(gst_err)?;
    for (i, endpoint) in endpoints.into_iter().enumerate() {
        let appsrc = make_appsrc(&format!("{name}-app-{i}"))?;
        bin.add(appsrc.upcast_ref::<gst::Element>())
            .map_err(gst_err)?;
        appsrc.link(&mixer).map_err(gst_err)?;
        pending.push(Pending {
            endpoint,
            appsrc,
            label: label.to_string(),
        });
    }
    let pad = mixer
        .static_pad("src")
        .ok_or_else(|| CaptureError::Gst("audiomixer has no src pad".into()))?;
    let ghost = gst::GhostPad::with_target(&pad).map_err(gst_err)?;
    bin.add_pad(&ghost).map_err(gst_err)?;
    Ok(bin.upcast())
}

/// `source → audiorate → F32LE 48 kHz stereo`. Returns the last element.
fn build_input(
    bin: &gst::Pipeline,
    input: &AudioInput,
    index: usize,
    pending: &mut Vec<Pending>,
) -> Result<gst::Element, CaptureError> {
    let src = make_input_source(
        &input.source,
        &format!("audio-input-{index}"),
        &input.label,
        pending,
    )?;
    let rate = make("audiorate")?;
    // Fill any gap with silence so AAC frames stay back to back.
    props::set_str(&rate, "skip-to-first", "true");
    let caps = capsfilter(&raw_caps())?;
    let chain = [src, rate, caps.clone()];
    bin.add_many(&chain).map_err(gst_err)?;
    gst::Element::link_many(&chain).map_err(gst_err)?;
    Ok(caps)
}

/// A running audio pipeline and its capture threads.
pub struct AudioSession {
    // Captures first: they stop (no more pushes) before the pipeline shuts down.
    captures: Vec<AudioCapture>,
    session: Option<CaptureSession>,
    labels: Vec<String>,
    /// (input label, estimator) of each per-process capture.
    lags: Vec<(String, SharedLag)>,
}

/// Measured timing of one per-process input (see `lag.rs`).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessLag {
    pub label: String,
    /// Current correction, ms.
    pub lag_ms: f64,
    /// Successful measurements so far (0: still the default).
    pub measurements: u64,
}

impl std::fmt::Debug for AudioSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSession")
            .field("labels", &self.labels)
            .finish_non_exhaustive()
    }
}

impl AudioSession {
    /// Build and start the audio pipeline. Its tracks ([`AudioConfig::track_labels`]) are
    /// numbered from `first_track`. Timestamps are on `timebase` (a new one if `None`).
    pub fn start(
        cfg: &AudioConfig,
        first_track: usize,
        sink: Arc<dyn PacketSink>,
        timebase: Option<&TimeBase>,
    ) -> Result<AudioSession, CaptureError> {
        klip_probe::init().map_err(gst_err)?;
        cfg.validate()?;
        let tb = match timebase {
            Some(t) => t.clone(),
            None => TimeBase::new()?,
        };
        let pipeline = gst::Pipeline::with_name("klip-audio");
        let stats = Arc::new(CaptureStats::default());
        let timing = Timing::Shift(
            aac_encoder()
                .map(aac_encoder_delay)
                .unwrap_or(0)
                .saturating_neg(),
        );
        let attach = |appsink: &gst_app::AppSink, k: usize| -> Result<(), CaptureError> {
            let track = first_track
                .checked_add(k)
                .ok_or_else(|| CaptureError::Config("track index".into()))?;
            attach_sink_with(appsink, track, sink.clone(), stats.clone(), timing);
            Ok(())
        };
        let mut pending = Vec::new();
        match cfg.inputs.as_slice() {
            [one] => {
                let last = build_input(&pipeline, one, 0, &mut pending)?;
                let (first, appsink) = build_aac_chain(&pipeline, cfg.bitrate)?;
                last.link(&first).map_err(gst_err)?;
                attach(&appsink, 0)?;
            }
            many => {
                let mixer = make("audiomixer")?;
                props::set_str(&mixer, "start-time-selection", "first");
                props::set_str(&mixer, "ignore-inactive-pads", "true");
                pipeline.add(&mixer).map_err(gst_err)?;
                let (first, appsink) = build_aac_chain(&pipeline, cfg.bitrate)?;
                mixer.link(&first).map_err(gst_err)?;
                attach(&appsink, 0)?;
                for (i, input) in many.iter().enumerate() {
                    let last = build_input(&pipeline, input, i, &mut pending)?;
                    let tee = make("tee")?;
                    props::set_str(&tee, "allow-not-linked", "true");
                    let to_mix = queue()?;
                    pipeline.add_many([&tee, &to_mix]).map_err(gst_err)?;
                    gst::Element::link_many([&last, &tee, &to_mix]).map_err(gst_err)?;
                    to_mix.link(&mixer).map_err(gst_err)?;
                    if cfg.separate_tracks {
                        let to_enc = queue()?;
                        pipeline.add(&to_enc).map_err(gst_err)?;
                        let (first, appsink) = build_aac_chain(&pipeline, cfg.bitrate)?;
                        gst::Element::link_many([&tee, &to_enc, &first]).map_err(gst_err)?;
                        attach(&appsink, i.saturating_add(1))?;
                    }
                }
            }
        }
        let session = CaptureSession::run(pipeline, sink.clone(), stats, (0, 0), Some(&tb))?;
        let offset = qpc_offset(&tb);
        let channels = u16::try_from(CHANNELS).unwrap_or(2);
        let mut captures = Vec::with_capacity(pending.len().saturating_add(1));
        let mut lags = Vec::new();
        for p in pending {
            let aligner = DriftAligner::new(SAMPLE_RATE)
                .ok_or_else(|| CaptureError::Config("sample rate".into()))?;
            let lag = matches!(p.endpoint, CaptureEndpoint::Process { .. })
                .then(|| Arc::new(Mutex::new(LagEstimator::new())));
            if let Some(l) = &lag {
                lags.push((p.label.clone(), l.clone()));
            }
            let pusher = Pusher {
                appsrc: p.appsrc,
                aligner,
                offset,
                label: p.label,
                sink: sink.clone(),
                lag,
            };
            captures.push(AudioCapture::start(
                p.endpoint,
                SAMPLE_RATE,
                channels,
                Box::new(pusher),
            )?);
        }
        if !lags.is_empty() {
            // The timing reference: the default output's mix, analysed in memory only.
            let reference = Reference {
                estimators: lags.iter().map(|(_, l)| l.clone()).collect(),
            };
            captures.push(AudioCapture::start(
                CaptureEndpoint::Loopback { device: None },
                SAMPLE_RATE,
                channels,
                Box::new(reference),
            )?);
        }
        let labels = cfg.track_labels();
        tracing::info!(tracks = ?labels, inputs = ?cfg.inputs.iter().map(|i| i.source.describe()).collect::<Vec<_>>(), "audio started");
        Ok(AudioSession {
            captures,
            session: Some(session),
            labels,
            lags,
        })
    }

    pub fn stats(&self) -> Option<&CaptureStats> {
        self.session.as_ref().map(CaptureSession::stats)
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Measured lag of every per-process input.
    pub fn process_lags(&self) -> Vec<ProcessLag> {
        self.lags
            .iter()
            .map(|(label, l)| {
                let l = l.lock().unwrap_or_else(PoisonError::into_inner);
                ProcessLag {
                    label: label.clone(),
                    lag_ms: l.lag() as f64 / 1e6,
                    measurements: l.measurements(),
                }
            })
            .collect()
    }

    /// Stop the captures, then the pipeline.
    pub fn stop(mut self) {
        for c in self.captures.drain(..) {
            c.stop();
        }
        if let Some(s) = self.session.take() {
            s.stop();
        }
    }
}

/// An audio device that can be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// Pass as `device` in [`AudioSource`].
    pub id: String,
    pub name: String,
    /// An output device (captured through loopback) rather than an input.
    pub loopback: bool,
}

/// Audio devices, inputs and outputs (for loopback), as listed by GStreamer's device
/// monitor. Only real endpoints are listed (their ids are Windows endpoint ids); the
/// default device is what you get without an id.
pub fn list_devices() -> Result<Vec<AudioDevice>, CaptureError> {
    klip_probe::init().map_err(gst_err)?;
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::from_str("audio/x-raw").map_err(gst_err)?;
    monitor.add_filter(Some("Audio/Source"), Some(&caps));
    monitor.start().map_err(gst_err)?;
    let devices = monitor.devices();
    monitor.stop();
    let mut out = Vec::new();
    for d in devices {
        let Some(p) = d.properties() else { continue };
        if p.get::<&str>("device.api").ok() != Some("wasapi2") {
            continue;
        }
        let Ok(id) = p.get::<String>("device.id") else {
            continue;
        };
        // "Default device" entries have GStreamer-only ids.
        if !id.contains("}.{") {
            continue;
        }
        out.push(AudioDevice {
            id,
            name: d.display_name().to_string(),
            loopback: p.get::<bool>("wasapi2.device.loopback").unwrap_or(false),
        });
    }
    Ok(out)
}

/// Whether this system supports per-process audio capture.
pub fn process_loopback_supported() -> bool {
    klip_platform::process_loopback_supported()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(label: &str) -> AudioInput {
        AudioInput {
            label: label.into(),
            source: AudioSource::System { device: None },
        }
    }

    #[test]
    fn track_layout() {
        let mut cfg = AudioConfig {
            inputs: vec![input("Game")],
            bitrate: DEFAULT_BITRATE,
            separate_tracks: true,
        };
        assert_eq!(cfg.track_labels(), ["Game"]);
        cfg.inputs.push(input("Microphone"));
        assert_eq!(cfg.track_labels(), ["Mix", "Game", "Microphone"]);
        cfg.separate_tracks = false;
        assert_eq!(cfg.track_labels(), ["Mix"]);
        assert!(cfg.validate().is_ok());
        cfg.bitrate = 100_000;
        assert!(cfg.validate().is_err());
        cfg.bitrate = DEFAULT_BITRATE;
        cfg.inputs.clear();
        assert!(cfg.validate().is_err());
        assert!(cfg.track_labels().is_empty());
        cfg.inputs = vec![input("x"); MAX_INPUTS + 1];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn aac_caps_to_track_info() {
        klip_probe::init().unwrap();
        let caps = gst::Caps::builder("audio/mpeg")
            .field("mpegversion", 4i32)
            .field("stream-format", "raw")
            .field("rate", 48_000i32)
            .field("channels", 2i32)
            .field("codec_data", gst::Buffer::from_slice([0x11u8, 0x90]))
            .build();
        let info = crate::track_info_from_caps(&caps).unwrap();
        assert_eq!(info.codec, klip_media::Codec::Aac);
        assert_eq!(&info.codec_data[..], &[0x11, 0x90]);
        assert_eq!(
            info.params,
            klip_media::TrackParams::Audio(klip_media::AudioParams {
                sample_rate: 48_000,
                channels: 2
            })
        );
        let adts = gst::Caps::builder("audio/mpeg")
            .field("mpegversion", 4i32)
            .field("stream-format", "adts")
            .field("rate", 48_000i32)
            .field("channels", 2i32)
            .build();
        assert!(crate::track_info_from_caps(&adts).is_err());
        let zero = gst::Caps::builder("audio/mpeg")
            .field("mpegversion", 4i32)
            .field("stream-format", "raw")
            .field("rate", 0i32)
            .field("channels", 2i32)
            .field("codec_data", gst::Buffer::from_slice([0x11u8, 0x90]))
            .build();
        assert!(crate::track_info_from_caps(&zero).is_err());
    }
}
