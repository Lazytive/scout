//! Encoder probes in a child process.
//!
//! A probe opens a real encoder session through the GPU driver. Doing that inside the
//! recorder risks the recorder (a driver that crashes or hangs takes the process with
//! it), and on the dev machine the outcome of an in-process NVENC probe depended on what
//! the process had done before (ADR 0009). So the app runs each probe in a fresh copy of
//! itself: the host binary calls [`worker_main`] first thing in `main`, and
//! [`Prober::Isolated`] starts `<exe> --klip-encoder-probe <element>` and reads one JSON
//! line back. The child's output is untrusted: size-capped and parsed strictly.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{EncoderChoice, ProbeResult, allowed, probe};

/// First argument that turns the host binary into a probe worker.
pub const WORKER_FLAG: &str = "--klip-encoder-probe";

/// Largest worker reply read.
const MAX_REPLY: u64 = 8 * 1024;

/// How probes are run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prober {
    /// In this process (tests, tools).
    InProcess,
    /// In a child process: this executable must call [`worker_main`] at start.
    Isolated(PathBuf),
}

impl Prober {
    /// Probe `choice`, never panicking; failures are in the result.
    pub fn probe(&self, choice: &EncoderChoice, timeout: Duration) -> ProbeResult {
        match self {
            Prober::InProcess => probe(choice, timeout),
            Prober::Isolated(exe) => probe_isolated(exe, choice, timeout),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

fn failed(choice: &EncoderChoice, start: Instant, error: String) -> ProbeResult {
    ProbeResult {
        choice: choice.clone(),
        ok: false,
        error: Some(error),
        elapsed: start.elapsed(),
    }
}

fn probe_isolated(exe: &PathBuf, choice: &EncoderChoice, timeout: Duration) -> ProbeResult {
    let start = Instant::now();
    let mut cmd = Command::new(exe);
    cmd.arg(WORKER_FLAG)
        .arg(&choice.element)
        .arg(timeout.as_millis().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console window flashes up when the host is a GUI app.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failed(choice, start, format!("could not start probe worker: {e}")),
    };
    // Generous margin over the probe's own timeout for process start-up and GStreamer
    // init (registry load).
    let deadline = start
        .checked_add(timeout.saturating_add(Duration::from_secs(10)))
        .unwrap_or(start);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return failed(choice, start, "probe worker timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                return failed(choice, start, format!("probe worker: {e}"));
            }
        }
    };
    let mut out = String::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(MAX_REPLY).read_to_string(&mut out);
    }
    let reply = out
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Reply>(l.trim()).ok());
    match reply {
        Some(r) => ProbeResult {
            choice: choice.clone(),
            ok: r.ok && status.success(),
            error: if r.ok {
                None
            } else {
                Some(
                    r.error
                        .unwrap_or_else(|| "failed".into())
                        .chars()
                        .take(300)
                        .collect(),
                )
            },
            elapsed: start.elapsed(),
        },
        None => failed(
            choice,
            start,
            format!("probe worker exited without a result ({status})"),
        ),
    }
}

/// If this process was started as a probe worker, run the probe, print the reply and
/// return the exit code; otherwise `None`. Call at the very start of `main`, before
/// anything else touches GStreamer.
pub fn worker_main() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(WORKER_FLAG) {
        return None;
    }
    let element = args.next().unwrap_or_default();
    let timeout = args
        .next()
        .and_then(|t| t.parse::<u64>().ok())
        .map_or(Duration::from_secs(10), |ms| {
            Duration::from_millis(ms.clamp(100, 60_000))
        });
    let reply = match run_worker(&element, timeout) {
        Ok(()) => Reply {
            ok: true,
            error: None,
        },
        Err(e) => Reply {
            ok: false,
            error: Some(e),
        },
    };
    let line = serde_json::to_string(&reply).unwrap_or_else(|_| "{\"ok\":false}".into());
    println!("{line}");
    Some(0)
}

fn run_worker(element: &str, timeout: Duration) -> Result<(), String> {
    let a = allowed(element).ok_or("not an allowed encoder")?;
    crate::init().map_err(|e| e.to_string())?;
    let factory = gstreamer::ElementFactory::find(element).ok_or("encoder is not installed")?;
    let choice = EncoderChoice {
        element: element.to_string(),
        codec: a.codec,
        family: a.family,
        vendor: a.vendor,
        description: factory.longname().to_string(),
        rank: 0,
        gpu: None,
        device_index: 0,
    };
    let r = probe(&choice, timeout);
    if r.ok {
        Ok(())
    } else {
        Err(r.error.unwrap_or_else(|| "failed".into()))
    }
}
