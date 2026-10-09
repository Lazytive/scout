//! Library thumbnails in a child process (ADR 0015).
//!
//! The clip folder can hold any file. A demuxer or decoder that crashes or hangs on one
//! must not take the app (and the replay buffer of a running game) down with it, so
//! posters and hover-scrub strips are made by a copy of the app started with
//! [`WORKER_FLAG`]: one [`ThumbJob`] per JSON line on its stdin, one [`ThumbReply`] per
//! line on its stdout. The host binary calls [`worker_main`] first thing in `main`. A job
//! that takes longer than its timeout gets the worker killed; the next job starts a new
//! one. An idle worker is stopped, so it holds no memory between batches. The worker's
//! output is untrusted: lines are size-capped and parsed strictly.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// First argument that turns the host binary into a media worker.
pub const WORKER_FLAG: &str = "--klip-media-worker";
/// Longest line read from either side.
const MAX_LINE: u64 = 64 * 1024;
/// A worker unused this long is stopped.
pub const IDLE_STOP: Duration = Duration::from_secs(20);
/// Where on the clip the poster is taken: this long before its end (the hotkey moment).
pub const POSTER_BACK: Duration = Duration::from_millis(500);

/// Make a clip's poster and/or strip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThumbJob {
    pub id: u64,
    pub clip: PathBuf,
    /// Write the poster here (atomically), if set.
    pub poster: Option<PathBuf>,
    pub poster_width: u32,
    /// Write the strip here (atomically), if set.
    pub strip: Option<PathBuf>,
    pub tiles: u32,
    pub tile_width: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThumbReply {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let r = std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// Do a job in this process. Both pictures are tried; errors are joined.
pub fn run_job(job: &ThumbJob) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Some(out) = &job.poster {
        let r = klip_capture::thumbnail::jpeg_near_end(&job.clip, POSTER_BACK, job.poster_width)
            .map_err(|e| e.to_string())
            .and_then(|jpeg| write_atomic(out, &jpeg).map_err(|e| e.to_string()));
        if let Err(e) = r {
            errors.push(format!("poster: {e}"));
        }
    }
    if let Some(out) = &job.strip {
        let r = klip_capture::thumbnail::strip(&job.clip, job.tiles, job.tile_width)
            .map_err(|e| e.to_string())
            .and_then(|s| write_atomic(out, &s.jpeg).map_err(|e| e.to_string()));
        if let Err(e) = r {
            errors.push(format!("strip: {e}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn run_job_caught(job: &ThumbJob) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(job)))
        .unwrap_or_else(|_| Err("thumbnail job panicked".into()))
}

/// One line, at most [`MAX_LINE`] bytes; `None` at the end of the stream. A longer line
/// is cut (and won't parse).
fn read_line<R: BufRead>(r: &mut R) -> Option<String> {
    let mut buf = Vec::new();
    match r.by_ref().take(MAX_LINE).read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(String::from_utf8_lossy(&buf).into_owned()),
    }
}

/// If this process was started as a media worker, serve jobs until stdin closes and
/// return the exit code; otherwise `None`. Call at the very start of `main`.
pub fn worker_main() -> Option<i32> {
    if std::env::args().nth(1).as_deref() != Some(WORKER_FLAG) {
        return None;
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    while let Some(line) = read_line(&mut input) {
        let reply = match serde_json::from_str::<ThumbJob>(line.trim()) {
            Ok(job) => {
                let r = run_job_caught(&job);
                ThumbReply {
                    id: job.id,
                    ok: r.is_ok(),
                    error: r.err(),
                }
            }
            Err(e) => ThumbReply {
                id: 0,
                ok: false,
                error: Some(format!("bad job: {e}")),
            },
        };
        let text = serde_json::to_string(&reply).unwrap_or_else(|_| "{}".into());
        let mut out = stdout.lock();
        if writeln!(out, "{text}").and_then(|()| out.flush()).is_err() {
            break;
        }
    }
    Some(0)
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<String>,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Closing stdin ends a healthy worker; kill covers one that is stuck.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs thumbnail jobs: in a child process (the app) or in this process (tests).
pub struct MediaWorker {
    /// The host binary; `None`: in this process.
    exe: Option<PathBuf>,
    running: Option<Running>,
    last_used: Instant,
}

impl MediaWorker {
    pub fn in_process() -> MediaWorker {
        MediaWorker {
            exe: None,
            running: None,
            last_used: Instant::now(),
        }
    }

    /// Jobs run in `<exe> --klip-media-worker`; `exe` must call [`worker_main`].
    pub fn isolated(exe: PathBuf) -> MediaWorker {
        MediaWorker {
            exe: Some(exe),
            running: None,
            last_used: Instant::now(),
        }
    }

    /// Run one job, waiting at most `timeout` for it.
    pub fn run(&mut self, job: &ThumbJob, timeout: Duration) -> Result<(), String> {
        let Some(exe) = &self.exe else {
            return run_job_caught(job);
        };
        self.last_used = Instant::now();
        if self.running.is_none() {
            self.running = Some(spawn(exe)?);
        }
        let r = match self.running.as_mut() {
            Some(w) => ask(w, job, timeout),
            None => Err(format!("{WORKER_FAILED} is missing")),
        };
        if r.as_ref().is_err_and(|e| e.starts_with(WORKER_FAILED)) {
            // Timed out, died or talked nonsense: start afresh next time.
            self.running = None;
        }
        self.last_used = Instant::now();
        r
    }

    /// Stop the worker if it has been unused for [`IDLE_STOP`].
    pub fn idle(&mut self) {
        if self.running.is_some() && self.last_used.elapsed() >= IDLE_STOP {
            self.running = None;
        }
    }

    /// A worker process is running.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }
}

/// Prefix of errors after which the worker is replaced.
const WORKER_FAILED: &str = "media worker";

fn spawn(exe: &Path) -> Result<Running, String> {
    let mut cmd = Command::new(exe);
    cmd.arg(WORKER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW (no console flashing up from a GUI app) and
        // BELOW_NORMAL_PRIORITY_CLASS (a game always comes first).
        cmd.creation_flags(0x0800_0000 | 0x0000_4000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{WORKER_FAILED} could not start: {e}"))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{WORKER_FAILED} has no pipes"));
    };
    let (tx, rx) = sync_channel::<String>(16);
    let reader = std::thread::Builder::new()
        .name("klip-media-worker-out".into())
        .spawn(move || {
            let mut out = BufReader::new(stdout);
            while let Some(line) = read_line(&mut out) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    if let Err(e) = reader {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{WORKER_FAILED}: no reader thread: {e}"));
    }
    Ok(Running {
        child,
        stdin,
        replies: rx,
    })
}

fn ask(w: &mut Running, job: &ThumbJob, timeout: Duration) -> Result<(), String> {
    let line = serde_json::to_string(job).map_err(|e| e.to_string())?;
    writeln!(w.stdin, "{line}")
        .and_then(|()| w.stdin.flush())
        .map_err(|e| format!("{WORKER_FAILED} is gone: {e}"))?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match w.replies.recv_timeout(left) {
            Ok(line) => match serde_json::from_str::<ThumbReply>(line.trim()) {
                Ok(r) if r.id == job.id => {
                    return if r.ok {
                        Ok(())
                    } else {
                        Err(r
                            .error
                            .unwrap_or_else(|| "failed".into())
                            .chars()
                            .take(300)
                            .collect())
                    };
                }
                // A stale reply (an earlier job that timed out can't be one: the worker
                // was replaced) or junk: keep waiting.
                _ => {}
            },
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!("{WORKER_FAILED} timed out"));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(format!("{WORKER_FAILED} exited"));
            }
        }
    }
}
