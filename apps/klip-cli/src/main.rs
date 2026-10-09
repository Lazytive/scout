//! `klip-cli`: run the replay-buffer recorder and drive it from other processes.
//!
//! - `klip-cli record --buffer 120s` captures (primary monitor, or a window) into the
//!   replay ring and serves the control channel.
//! - `klip-cli clip --last 30s` asks the running recorder to save a clip. Bind it to a
//!   desktop shortcut as a hotkey fallback.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
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
    Record(RecordArgs),
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

fn pick_encoder(codec: Codec, requested: Option<&str>) -> Result<klip_probe::EncoderChoice> {
    let vendors = gpu_vendors();
    match requested {
        Some(name) => klip_probe::candidates(codec, &vendors)
            .into_iter()
            .find(|c| c.element == name)
            .ok_or_else(|| {
                anyhow!(
                    "{name} is not an installed, allowed {} encoder",
                    codec.name()
                )
            }),
        None => Ok(klip_probe::best(codec, &vendors, Duration::from_secs(10))?),
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
    let encoder = pick_encoder(codec, args.encoder.as_deref())?;
    let target = pick_target(args)?;
    let clip_dir = match &args.out {
        Some(p) => p.clone(),
        None => dirs::video_dir()
            .or_else(dirs::home_dir)
            .ok_or_else(|| anyhow!("no Videos folder; pass --out"))?
            .join("Klip"),
    };
    let max_buffer_bytes = match args.max_buffer_mb {
        Some(mb) => mb.saturating_mul(1024 * 1024),
        None => estimate_buffer_bytes(args.buffer, args.bitrate),
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
        },
        encode: EncodeConfig {
            encoder: encoder.clone(),
            bitrate_kbps: args.bitrate,
            keyframe_interval: args.fps,
        },
    };
    let recorder = Arc::new(Recorder::start(cfg).context("could not start capture")?);
    let server = Server::start(recorder.clone(), Some(&discovery)).context("control channel")?;
    let status = recorder.status();
    println!(
        "{}",
        json!({
            "event": "ready",
            "encoder": encoder.element,
            "width": status.get("width"),
            "height": status.get("height"),
            "fps": args.fps,
            "buffer_ms": args.buffer / NANOS_PER_MILLI,
            "max_buffer_bytes": max_buffer_bytes,
            "clip_dir": clip_dir.to_string_lossy(),
            "port": server.port(),
        })
    );
    eprintln!(
        "Recording with {} at {}x{}. Save a clip with `klip-cli clip --last 30s`; stop with Ctrl+C.",
        encoder.element,
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
                    "GPU {}: {} ({}{})",
                    g.index,
                    g.name,
                    g.vendor.name(),
                    if g.software { ", software" } else { "" }
                );
            }
            let results = klip_probe::probe_all(codec, &gpu_vendors(), Duration::from_secs(10));
            if results.is_empty() {
                println!("No allowed {} encoders are installed.", codec.name());
            }
            for r in results {
                println!(
                    "{:<22} {:<4} {:>5} ms  {}{}",
                    r.choice.element,
                    if r.ok { "ok" } else { "FAIL" },
                    r.elapsed.as_millis(),
                    r.choice.description,
                    r.error.map(|e| format!(" ({e})")).unwrap_or_default()
                );
            }
            Ok(())
        }
        Cmd::Monitors => {
            klip_platform::enable_dpi_awareness();
            for (i, m) in klip_platform::list_monitors()?.into_iter().enumerate() {
                println!(
                    "{i}: {}x{} at ({}, {}){}",
                    m.width,
                    m.height,
                    m.x,
                    m.y,
                    if m.primary { " primary" } else { "" }
                );
            }
            Ok(())
        }
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

fn main() -> std::process::ExitCode {
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
