//! `klip-cli`: run the replay-buffer recorder and drive it from other processes.
//!
//! - `klip-cli record --buffer 120s` captures (primary monitor, or a window) into the
//!   replay ring and serves the control channel.
//! - `klip-cli clip --last 30s` asks the running recorder to save a clip. Bind it to a
//!   desktop shortcut as a hotkey fallback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use klip_capture::apps::{self, SystemPlan};
use klip_capture::audio::{AudioConfig, AudioInput, AudioSource};
use klip_capture::{EncodeConfig, VideoConfig, VideoTarget};
use klip_control::{Client, ClientError, Server, commands};
use klip_media::{Codec, NANOS_PER_MILLI, Nanos};
use klip_recorder::{Recorder, RecorderConfig, estimate_buffer_bytes};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(name = "klip-cli", version, about = "Klip game clipper: recorder CLI")]
struct Cli {
    /// Control-channel discovery file (default: per-user runtime dir).
    #[arg(long, global = true)]
    discovery: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the replay buffer until stopped (Ctrl+C or `klip-cli stop`).
    Record(Box<RecordArgs>),
    /// Save the last part of the replay buffer of the running recorder.
    Clip {
        /// Clip length, e.g. 30s, 2m.
        #[arg(long, default_value = "30s", value_parser = parse_dur)]
        last: Nanos,
        /// How long to wait for the recorder to answer.
        #[arg(long, default_value = "30s", value_parser = parse_dur)]
        timeout: Nanos,
    },
    /// Show the running recorder's status.
    Status,
    /// Stop the running recorder.
    Stop,
    /// List GPUs and test every allowed encoder for a codec.
    Probe {
        #[arg(long, default_value = "h264")]
        codec: String,
    },
    /// List monitors (index, size, primary).
    Monitors,
    /// List visible windows whose title contains TEXT.
    Windows { text: String },
    /// List audio devices (inputs, and outputs for system audio) with their ids.
    AudioDevices,
    /// List apps that have audio, and which are left out of system audio by default.
    AudioApps,
}

/// What the game-audio track records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CaptureApiArg {
    Auto,
    Klip,
    GstWgc,
    GstDxgi,
}

impl From<CaptureApiArg> for klip_capture::CaptureApi {
    fn from(a: CaptureApiArg) -> Self {
        match a {
            CaptureApiArg::Auto => klip_capture::CaptureApi::Auto,
            CaptureApiArg::Klip => klip_capture::CaptureApi::Klip,
            CaptureApiArg::GstWgc => klip_capture::CaptureApi::GstWgc,
            CaptureApiArg::GstDxgi => klip_capture::CaptureApi::GstDxgi,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum AudioMode {
    /// Only the captured window's process (and its children). Falls back to `system` when
    /// recording a monitor or on Windows older than 10 version 2004.
    Game,
    /// Everything the output devices play, except music players (see `audio-apps`).
    System,
    /// Only the apps given with --app.
    Apps,
    /// No game/system audio.
    None,
}

#[derive(clap::Args, Debug)]
struct RecordArgs {
    /// Replay buffer length.
    #[arg(long, default_value = "120s", value_parser = parse_dur)]
    buffer: Nanos,
    /// Hard cap on buffered video in MB (default: estimated from length and bitrate).
    #[arg(long)]
    max_buffer_mb: Option<u64>,
    /// Capture the first visible window whose title contains this text.
    #[arg(long, conflicts_with_all = ["window_handle", "monitor"])]
    window_title: Option<String>,
    /// Capture this window (HWND).
    #[arg(long, conflicts_with = "monitor")]
    window_handle: Option<u64>,
    /// Capture monitor N from `klip-cli monitors` (default: primary).
    #[arg(long)]
    monitor: Option<usize>,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Video bitrate in kbit/s.
    #[arg(long, default_value_t = 20_000)]
    bitrate: u32,
    /// Largest output size; bigger sources are scaled down keeping aspect ratio.
    #[arg(long, default_value = "1920x1080")]
    max_size: String,
    #[arg(long, default_value = "h264")]
    codec: String,
    /// Use this encoder element instead of probing (must be on the allowlist).
    #[arg(long)]
    encoder: Option<String>,
    /// Folder for clips (default: Videos/Klip).
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    no_cursor: bool,
    /// Video capture implementation: Klip's own WGC capture (falls back to GStreamer's
    /// when it can't start), only Klip's, GStreamer's WGC source, or GStreamer's DXGI
    /// Desktop Duplication (monitors only).
    #[arg(long, value_enum, default_value_t = CaptureApiArg::Auto)]
    capture_api: CaptureApiArg,
    /// Game/system audio (default: game when recording a window, else system).
    #[arg(long, value_enum)]
    audio: Option<AudioMode>,
    /// Also record a microphone: the default input, or a device id from
    /// `klip-cli audio-devices`.
    #[arg(long, num_args = 0..=1, default_missing_value = "default", value_name = "DEVICE")]
    mic: Option<String>,
    /// AAC bitrate per audio track in kbit/s (96, 128, 160, 192, 256 or 320).
    #[arg(long, default_value_t = 160)]
    audio_bitrate: u32,
    /// With game audio and a mic: write only the mix, not the separate tracks.
    #[arg(long)]
    no_separate_tracks: bool,
    /// Also record this app (executable name, e.g. Discord.exe) on its own track; repeat
    /// for more. With `--audio apps`, record only these.
    #[arg(long, value_name = "EXE")]
    app: Vec<String>,
    /// Leave this app out of system audio, in addition to the music players left out by
    /// default; repeat for more.
    #[arg(long, value_name = "EXE")]
    exclude_app: Vec<String>,
    /// Don't leave music players out of system audio.
    #[arg(long)]
    no_default_excludes: bool,
}

fn parse_dur(s: &str) -> Result<Nanos, String> {
    klip_media::parse_duration(s).ok_or_else(|| format!("invalid duration {s:?} (try 30s, 2m)"))
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| anyhow!("size must look like 1920x1080"))?;
    let (w, h): (u32, u32) = (w.trim().parse()?, h.trim().parse()?);
    if !(64..=8192).contains(&w) || !(64..=8192).contains(&h) {
        bail!("size out of range");
    }
    Ok((w, h))
}

fn discovery_path(cli: &Cli) -> Result<PathBuf> {
    match &cli.discovery {
        Some(p) => Ok(p.clone()),
        None => {
            klip_control::default_discovery_path().ok_or_else(|| anyhow!("no per-user data folder"))
        }
    }
}

fn to_duration(ns: Nanos) -> Duration {
    Duration::from_nanos(u64::try_from(ns).unwrap_or(0))
}

fn connect(cli: &Cli, timeout: Duration) -> Result<Client> {
    let path = discovery_path(cli)?;
    match Client::connect(&path, timeout) {
        Ok(c) => Ok(c),
        Err(ClientError::NotRunning) => {
            bail!("the Klip recorder is not running (start it with `klip-cli record`)")
        }
        Err(e) => Err(e.into()),
    }
}

fn gpu_vendors() -> Vec<klip_platform::GpuVendor> {
    match klip_platform::list_gpus() {
        Ok(g) => g
            .into_iter()
            .filter(|a| !a.software)
            .map(|a| a.vendor)
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not list GPUs");
            Vec::new()
        }
    }
}

/// Where probe results are remembered between runs.
fn encoder_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("Klip").join("encoders.json"))
}

/// The probe cache for this machine's current setup (empty if none or outdated).
fn load_encoder_cache() -> (klip_probe::ProbeCache, Option<PathBuf>) {
    let adapters: Vec<(u32, u32, Option<u64>)> = klip_platform::list_gpus()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| !g.software)
        .map(|g| (g.vendor_id, g.device_id, g.driver_version))
        .collect();
    let fp = klip_probe::fingerprint(&adapters, &klip_probe::installed_encoders());
    let path = encoder_cache_path();
    let cache = match &path {
        Some(p) => klip_probe::ProbeCache::load(p, &fp),
        None => klip_probe::ProbeCache::new(&fp),
    };
    (cache, path)
}

fn save_encoder_cache(cache: &klip_probe::ProbeCache, path: Option<&Path>) {
    if let Some(p) = path
        && let Err(e) = cache.save(p)
    {
        tracing::warn!(error = %e, "could not save the encoder cache");
    }
}

/// Vendor of the GPU that shows `target` (encoding there avoids copies between GPUs).
fn target_gpu(target: VideoTarget) -> Option<klip_platform::GpuVendor> {
    let monitors = klip_platform::list_monitors().ok()?;
    let handle = match target {
        VideoTarget::PrimaryMonitor => monitors.iter().find(|m| m.primary)?.handle,
        VideoTarget::Monitor(h) => h,
        VideoTarget::Window(h) => klip_platform::window_info(h).ok()?.monitor,
    };
    let luid = monitors.iter().find(|m| m.handle == handle)?.adapter_luid?;
    klip_platform::list_gpus()
        .ok()?
        .into_iter()
        .find(|g| g.luid == luid)
        .map(|g| g.vendor)
}

/// Probes run in a child copy of this binary, so a misbehaving driver can't take the
/// recorder down and in-process state can't skew the result.
fn prober() -> klip_probe::Prober {
    match std::env::current_exe() {
        Ok(exe) => klip_probe::Prober::Isolated(exe),
        Err(_) => klip_probe::Prober::InProcess,
    }
}

/// Encoders to try, best first. An explicitly requested encoder is used alone.
fn pick_encoders(
    codec: Codec,
    requested: Option<&str>,
    prefer: Option<klip_platform::GpuVendor>,
    cache: &mut klip_probe::ProbeCache,
) -> Result<Vec<klip_probe::EncoderChoice>> {
    let vendors = gpu_vendors();
    match requested {
        Some(name) => klip_probe::candidates(codec, &vendors, prefer)
            .into_iter()
            .find(|c| c.element == name)
            .map(|c| vec![c])
            .ok_or_else(|| {
                anyhow!(
                    "{name} is not an installed, allowed {} encoder",
                    codec.name()
                )
            }),
        None => {
            let list = klip_probe::ranked(
                codec,
                &vendors,
                prefer,
                cache,
                &prober(),
                Duration::from_secs(10),
            );
            if list.is_empty() {
                bail!("no working {} encoder found", codec.name());
            }
            Ok(list)
        }
    }
}

fn pick_target(args: &RecordArgs) -> Result<VideoTarget> {
    if let Some(text) = &args.window_title {
        let w = klip_platform::find_windows(text)?
            .into_iter()
            .find(|w| !w.minimized && w.client_width > 0)
            .ok_or_else(|| anyhow!("no visible window matches the given title"))?;
        return Ok(VideoTarget::Window(w.handle));
    }
    if let Some(h) = args.window_handle {
        klip_platform::window_info(h)?;
        return Ok(VideoTarget::Window(h));
    }
    if let Some(i) = args.monitor {
        let m = klip_platform::list_monitors()?
            .into_iter()
            .nth(i)
            .ok_or_else(|| anyhow!("no monitor {i}"))?;
        return Ok(VideoTarget::Monitor(m.handle));
    }
    Ok(VideoTarget::PrimaryMonitor)
}

fn excluded_apps(args: &RecordArgs) -> Vec<String> {
    let mut v: Vec<String> = if args.no_default_excludes {
        Vec::new()
    } else {
        apps::DEFAULT_EXCLUDED_APPS
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    v.extend(args.exclude_app.iter().cloned());
    v
}

/// System audio without the excluded apps (see `klip_capture::apps::plan_system`).
fn system_source(excluded: &[String]) -> AudioSource {
    let whole = AudioSource::System { device: None };
    if excluded.is_empty() || !klip_capture::audio::process_loopback_supported() {
        return whole;
    }
    let procs = match klip_platform::list_processes() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "process list unavailable; recording all system audio");
            return whole;
        }
    };
    let sessions = klip_platform::audio_sessions().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "audio sessions unavailable");
        Vec::new()
    });
    match apps::plan_system(&procs, &sessions, excluded) {
        SystemPlan::Whole => whole,
        SystemPlan::Except { pid, exe } => {
            eprintln!("Leaving {exe} out of system audio.");
            AudioSource::SystemExcept { pid }
        }
        SystemPlan::Apps { pids, excluded } => {
            eprintln!(
                "{} are playing; recording the other {} apps with audio one by one (apps that start later are not included).",
                excluded.join(", "),
                pids.len()
            );
            AudioSource::Apps { pids }
        }
    }
}

/// The audio inputs for a recording, or `None` for silence.
fn pick_audio(args: &RecordArgs, target: VideoTarget) -> Result<Option<AudioConfig>> {
    let mode = args.audio.unwrap_or(match target {
        VideoTarget::Window(_) => AudioMode::Game,
        VideoTarget::Monitor(_) | VideoTarget::PrimaryMonitor => AudioMode::System,
    });
    if mode == AudioMode::Apps && args.app.is_empty() {
        bail!("--audio apps needs at least one --app");
    }
    if mode == AudioMode::System && !args.app.is_empty() {
        bail!("system audio already includes every app; use --audio game or apps with --app");
    }
    let mut inputs = Vec::new();
    let system = || system_source(&excluded_apps(args));
    match (mode, target) {
        (AudioMode::None | AudioMode::Apps, _) => {}
        (AudioMode::Game, VideoTarget::Window(h)) => {
            let pid = klip_platform::window_info(h)?.pid;
            let source = if klip_capture::audio::process_loopback_supported() && pid != 0 {
                AudioSource::Process { pid }
            } else {
                eprintln!(
                    "Per-application audio isn't available here; recording all system audio."
                );
                AudioSource::System { device: None }
            };
            inputs.push(AudioInput {
                label: "Game".into(),
                source,
            });
        }
        (AudioMode::Game | AudioMode::System, _) => inputs.push(AudioInput {
            label: "System".into(),
            source: system(),
        }),
    }
    if !args.app.is_empty() {
        if !klip_capture::audio::process_loopback_supported() {
            bail!("recording single apps needs Windows 10 version 2004 or newer");
        }
        let roots = apps::app_roots(&klip_platform::list_processes()?, &args.app);
        let pids: Vec<u32> = roots.iter().map(|(pid, _)| *pid).collect();
        let source = match pids.as_slice() {
            [] => bail!("none of the --app programs is running"),
            [pid] => AudioSource::Process { pid: *pid },
            _ => AudioSource::Apps { pids },
        };
        inputs.push(AudioInput {
            label: apps::label_for(&args.app),
            source,
        });
    }
    if let Some(mic) = &args.mic {
        inputs.push(AudioInput {
            label: "Microphone".into(),
            source: AudioSource::Device {
                device: (mic != "default").then(|| mic.clone()),
            },
        });
    }
    if inputs.is_empty() {
        return Ok(None);
    }
    let bitrate = args.audio_bitrate.saturating_mul(1000);
    if !klip_capture::audio::BITRATES.contains(&bitrate) {
        bail!("--audio-bitrate must be one of 96, 128, 160, 192, 256, 320");
    }
    Ok(Some(AudioConfig {
        inputs,
        bitrate,
        separate_tracks: !args.no_separate_tracks,
    }))
}

fn record(cli: &Cli, args: &RecordArgs) -> Result<()> {
    let discovery = discovery_path(cli)?;
    if let Ok(mut c) = Client::connect(&discovery, Duration::from_millis(500))
        && c.call(commands::PING, json!(null)).is_ok()
    {
        bail!("a Klip recorder is already running");
    }
    klip_probe::init()?;
    let codec =
        Codec::from_name(&args.codec).ok_or_else(|| anyhow!("unknown codec {}", args.codec))?;
    if !matches!(codec, Codec::H264 | Codec::H265 | Codec::Av1) {
        bail!("{} is not a video codec", codec.name());
    }
    if !(1..=500).contains(&args.fps) {
        bail!("--fps must be 1..=500");
    }
    if !(500..=500_000).contains(&args.bitrate) {
        bail!("--bitrate must be 500..=500000 kbit/s");
    }
    let (max_w, max_h) = parse_size(&args.max_size)?;
    let target = pick_target(args)?;
    let (mut cache, cache_path) = load_encoder_cache();
    let mut encoders = pick_encoders(
        codec,
        args.encoder.as_deref(),
        target_gpu(target),
        &mut cache,
    )?;
    save_encoder_cache(&cache, cache_path.as_deref());
    let encoder = encoders.remove(0);
    let clip_dir = match &args.out {
        Some(p) => p.clone(),
        None => dirs::video_dir()
            .or_else(dirs::home_dir)
            .ok_or_else(|| anyhow!("no Videos folder; pass --out"))?
            .join("Klip"),
    };
    let audio = pick_audio(args, target)?;
    let audio_kbps = audio.as_ref().map_or(0, |a| {
        let tracks = u32::try_from(a.track_labels().len()).unwrap_or(u32::MAX);
        (a.bitrate / 1000).saturating_mul(tracks)
    });
    let max_buffer_bytes = match args.max_buffer_mb {
        Some(mb) => mb.saturating_mul(1024 * 1024),
        None => estimate_buffer_bytes(args.buffer, args.bitrate.saturating_add(audio_kbps)),
    };
    let cfg = RecorderConfig {
        buffer: args.buffer,
        max_buffer_bytes,
        clip_dir: clip_dir.clone(),
        video: VideoConfig {
            target,
            fps: args.fps,
            max_width: max_w,
            max_height: max_h,
            show_cursor: !args.no_cursor,
            api: args.capture_api.into(),
            output: None,
        },
        encode: EncodeConfig {
            encoder,
            bitrate_kbps: args.bitrate,
            keyframe_interval: args.fps,
        },
        encoder_fallbacks: encoders,
        audio,
    };
    let recorder = Recorder::start(cfg).context("could not start capture")?;
    for a in recorder.encoder_attempts() {
        match &a.error {
            None => cache.record_started(&a.element),
            Some(e) => {
                eprintln!("Encoder {} failed to start: {e}", a.element);
                cache.record_start_failure(&a.element, e);
            }
        }
    }
    save_encoder_cache(&cache, cache_path.as_deref());
    let recorder = Arc::new(recorder);
    let server = Server::start(recorder.clone(), Some(&discovery)).context("control channel")?;
    let status = recorder.status();
    println!(
        "{}",
        json!({
            "event": "ready",
            "encoder": status.get("encoder"),
            "width": status.get("width"),
            "height": status.get("height"),
            "fps": args.fps,
            "buffer_ms": args.buffer / NANOS_PER_MILLI,
            "max_buffer_bytes": max_buffer_bytes,
            "clip_dir": clip_dir.to_string_lossy(),
            "port": server.port(),
            "audio": status.get("audio"),
        })
    );
    if let Some(e) = status
        .get("audio")
        .and_then(|a| a.get("error"))
        .and_then(|e| e.as_str())
    {
        eprintln!("Audio is not recording: {e}");
    }
    eprintln!(
        "Recording with {} at {}x{}. Save a clip with `klip-cli clip --last 30s`; stop with Ctrl+C.",
        status
            .get("encoder")
            .and_then(|v| v.as_str())
            .unwrap_or("?"),
        status.get("width").and_then(|v| v.as_u64()).unwrap_or(0),
        status.get("height").and_then(|v| v.as_u64()).unwrap_or(0),
    );

    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = interrupted.clone();
        ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))
            .context("Ctrl+C handler")?;
    }
    while !interrupted.load(Ordering::Relaxed) && !recorder.stop_requested() {
        std::thread::sleep(Duration::from_millis(100));
    }
    server.shutdown();
    recorder.stop_capture();
    eprintln!("Stopped.");
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Record(args) => {
            klip_platform::enable_dpi_awareness();
            record(&cli, args)
        }
        Cmd::Clip { last, timeout } => {
            let mut c = connect(&cli, to_duration(*timeout))?;
            let last_ms = last / NANOS_PER_MILLI;
            let r = c.call(commands::CLIP_SAVE, json!({ "last_ms": last_ms }))?;
            println!("{r}");
            Ok(())
        }
        Cmd::Status => {
            let r = connect(&cli, Duration::from_secs(5))?
                .call(commands::RECORDER_STATUS, json!(null))?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }
        Cmd::Stop => {
            connect(&cli, Duration::from_secs(5))?.call(commands::RECORDER_STOP, json!(null))?;
            eprintln!("Recorder is stopping.");
            Ok(())
        }
        Cmd::Probe { codec } => {
            klip_probe::init()?;
            let codec = Codec::from_name(codec).ok_or_else(|| anyhow!("unknown codec {codec}"))?;
            for g in klip_platform::list_gpus()? {
                println!(
                    "GPU {}: {} ({}{}, LUID {:#x})",
                    g.index,
                    g.name,
                    g.vendor.name(),
                    if g.software { ", software" } else { "" },
                    g.luid
                );
            }
            let prefer = target_gpu(VideoTarget::PrimaryMonitor);
            let results = klip_probe::probe_all(
                codec,
                &gpu_vendors(),
                prefer,
                &prober(),
                Duration::from_secs(10),
            );
            if results.is_empty() {
                println!("No allowed {} encoders are installed.", codec.name());
            }
            // A full probe refreshes the remembered results.
            let (mut cache, cache_path) = load_encoder_cache();
            for r in &results {
                cache.record(r);
                println!(
                    "{:<22} {:<4} {:>5} ms  {}{}{}",
                    r.choice.element,
                    if r.ok { "ok" } else { "FAIL" },
                    r.elapsed.as_millis(),
                    r.choice.description,
                    r.choice
                        .gpu
                        .map(|g| format!(" [{}]", g.name()))
                        .unwrap_or_default(),
                    r.error
                        .as_ref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                );
            }
            save_encoder_cache(&cache, cache_path.as_deref());
            Ok(())
        }
        Cmd::Monitors => {
            klip_platform::enable_dpi_awareness();
            let gpus = klip_platform::list_gpus().unwrap_or_default();
            for (i, m) in klip_platform::list_monitors()?.into_iter().enumerate() {
                let gpu = m
                    .adapter_luid
                    .and_then(|l| gpus.iter().find(|g| g.luid == l))
                    .map(|g| g.name.as_str())
                    .unwrap_or("unknown GPU");
                let hdr = if m.hdr {
                    format!(
                        ", HDR (SDR white {} nits, peak {} nits)",
                        m.sdr_white_nits.map_or("?".into(), |n| format!("{n:.0}")),
                        m.max_nits.map_or("?".into(), |n| format!("{n:.0}"))
                    )
                } else {
                    String::new()
                };
                println!(
                    "{i}: {}x{} at ({}, {}){} on {gpu}{hdr}",
                    m.width,
                    m.height,
                    m.x,
                    m.y,
                    if m.primary { " primary" } else { "" }
                );
            }
            Ok(())
        }
        Cmd::AudioDevices => audio_devices(),
        Cmd::AudioApps => audio_apps(),
        Cmd::Windows { text } => {
            klip_platform::enable_dpi_awareness();
            for w in klip_platform::find_windows(text)? {
                println!(
                    "{:#x}  pid {}  {}x{}{}  {}",
                    w.handle,
                    w.pid,
                    w.client_width,
                    w.client_height,
                    if w.minimized { " (minimised)" } else { "" },
                    w.title
                );
            }
            Ok(())
        }
    }
}

fn audio_devices() -> Result<()> {
    let devices = klip_capture::audio::list_devices()?;
    if devices.is_empty() {
        println!("No audio devices found.");
    }
    for (title, loopback) in [("Inputs (--mic DEVICE)", false), ("Outputs", true)] {
        println!("{title}:");
        for d in devices.iter().filter(|d| d.loopback == loopback) {
            println!("  {}\n    {}", d.name, d.id);
        }
    }
    println!("Without an id, the default device (when recording starts) is used.");
    Ok(())
}

fn audio_apps() -> Result<()> {
    let procs = klip_platform::list_processes()?;
    let sessions = klip_platform::audio_sessions()?;
    if sessions.is_empty() {
        println!("No app has audio right now.");
        return Ok(());
    }
    let excluded: Vec<String> = apps::DEFAULT_EXCLUDED_APPS
        .iter()
        .map(ToString::to_string)
        .collect();
    let excluded_roots = apps::app_roots(&procs, &excluded);
    println!("{:>7}  {:<8} app", "pid", "playing");
    for s in sessions {
        let exe = procs
            .iter()
            .find(|p| p.pid == s.pid)
            .map_or("?", |p| p.exe.as_str());
        // A session can belong to a child process of a music player.
        let left_out = excluded.iter().any(|e| e.eq_ignore_ascii_case(exe))
            || excluded_roots.iter().any(|(pid, _)| *pid == s.pid);
        println!(
            "{:>7}  {:<8} {exe}{}",
            s.pid,
            if s.active { "yes" } else { "no" },
            if left_out {
                "  (left out of system audio by default)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    // Encoder probes run in a fresh copy of this binary (klip_probe::isolated).
    if let Some(code) = klip_probe::worker_main() {
        return std::process::ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
