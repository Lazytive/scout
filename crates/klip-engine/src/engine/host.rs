//! Where the recorder runs (ADR 0016): in a child process of the app's executable (see
//! [`crate::worker`]), or in the engine's own process (tests, tools). The engine sees
//! one [`RecorderHandle`] either way.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use klip_control::{CommandError, Handler, Request, Response, commands};
use klip_probe::EncoderChoice;
use klip_recorder::{EncoderAttempt, Recorder};
use klip_settings::Settings;
use serde_json::Value;

use super::lock;
use crate::games::DetectedGame;
use crate::worker::{self, StartLine, StartReply, StartRequest};
use crate::{EngineError, config};

/// Where recorders run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Isolation {
    /// In the engine's process.
    InProcess,
    /// In child processes of this executable, which calls [`crate::worker_main`] first
    /// thing in `main`.
    Child(PathBuf),
}

impl Isolation {
    /// Child processes when this executable hosts the workers, else in process.
    pub fn auto() -> Isolation {
        if worker::hosts_workers()
            && let Ok(exe) = std::env::current_exe()
        {
            return Isolation::Child(exe);
        }
        Isolation::InProcess
    }
}

/// Longest line read from a recorder process.
const MAX_REPLY_LINE: usize = 1024 * 1024;
/// A recorder process must be running (or have given up) by then. Generous: encoder
/// fallbacks take up to 3 s each, and on Linux the desktop may ask the user which
/// screen to share.
const START_TIMEOUT: Duration = Duration::from_secs(180);
/// Status reads; a recorder that doesn't answer in time is reported, not waited for.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// Saves and stopping a recording write files (up to the whole buffer or a long
/// recording's index).
const WRITE_TIMEOUT: Duration = Duration::from_secs(600);
const OTHER_TIMEOUT: Duration = Duration::from_secs(30);
/// After [`worker::FINISH`]: time to finish saves in flight and exit.
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// What a started recorder reports.
pub(crate) struct Started {
    pub handle: RecorderHandle,
    pub clip_dir: PathBuf,
    pub fps: u32,
    pub buffer_ms: i64,
    pub status: Value,
    pub attempts: Vec<EncoderAttempt>,
}

/// How a recorder ended.
pub(crate) struct Stopped {
    /// Summary of the recording that was running.
    pub summary: Option<Value>,
    /// The process didn't stop cleanly: recordings it left may need finalising.
    pub unclean: bool,
}

/// A running recorder, here or in a child process.
pub(crate) enum RecorderHandle {
    Local(Box<Recorder>),
    Child(ChildRecorder),
}

/// Start a recorder of `game` (or the desktop) with `encoders` (best first).
pub(crate) fn start(
    iso: &Isolation,
    settings: &Settings,
    game: Option<&DetectedGame>,
    encoders: Vec<EncoderChoice>,
) -> Result<Started, EngineError> {
    match iso {
        Isolation::InProcess => {
            let plan = config::plan_with(settings, game, encoders)?;
            let buffer_ms = plan
                .config
                .buffer
                .checked_div(klip_media::NANOS_PER_MILLI)
                .unwrap_or(0);
            let rec = Recorder::start(plan.config)?;
            Ok(Started {
                status: rec.status(),
                attempts: rec.encoder_attempts().to_vec(),
                handle: RecorderHandle::Local(Box::new(rec)),
                clip_dir: plan.clip_dir,
                fps: plan.fps,
                buffer_ms,
            })
        }
        Isolation::Child(exe) => ChildRecorder::start(exe, settings, game, &encoders),
    }
}

impl RecorderHandle {
    /// Run a recorder command (`clip.save`, `session.*`, `bookmark.add`,
    /// `recorder.status`).
    pub fn handle(&self, cmd: &str, args: &Value) -> Result<Value, CommandError> {
        match self {
            RecorderHandle::Local(r) => r.handle(cmd, args),
            RecorderHandle::Child(c) => {
                let timeout = match cmd {
                    commands::RECORDER_STATUS => STATUS_TIMEOUT,
                    commands::CLIP_SAVE | commands::SESSION_STOP | commands::SESSION_TOGGLE => {
                        WRITE_TIMEOUT
                    }
                    _ => OTHER_TIMEOUT,
                };
                c.call(cmd, args, timeout)
            }
        }
    }

    /// Recorder status and whether a full-session recording runs.
    pub fn status(&self) -> Result<(Value, bool), CommandError> {
        match self {
            RecorderHandle::Local(r) => Ok((r.status(), r.session_running())),
            RecorderHandle::Child(c) => {
                let v = c.call(worker::STATUS, &Value::Null, STATUS_TIMEOUT)?;
                Ok((
                    v.get("status").cloned().unwrap_or(Value::Null),
                    v.get("session_running").and_then(Value::as_bool) == Some(true),
                ))
            }
        }
    }

    /// The recorder's process ended without being asked to (crash, killed): why.
    pub fn ended(&self) -> Option<String> {
        match self {
            RecorderHandle::Local(_) => None,
            RecorderHandle::Child(c) => c.ended(),
        }
    }

    /// The recorder's process id, if it has its own process.
    pub fn pid(&self) -> Option<u32> {
        match self {
            RecorderHandle::Local(_) => None,
            RecorderHandle::Child(c) => Some(c.pid),
        }
    }

    /// Stop capturing and finalise a running recording; a child process exits (its GPU
    /// memory with it) before this returns.
    pub fn stop(&self) -> Stopped {
        match self {
            RecorderHandle::Local(r) => Stopped {
                summary: r.stop_capture().map(|s| s.to_json()),
                unclean: false,
            },
            RecorderHandle::Child(c) => c.finish(),
        }
    }
}

/// Start a worker process of `exe`; its stderr (logs) is passed on to ours.
fn spawn_worker(exe: &Path, args: &[&std::ffi::OsStr], stdin: Stdio) -> std::io::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console window for the worker when the host has none.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn()?;
    if let Some(mut err) = child.stderr.take() {
        let _ = std::thread::Builder::new()
            .name("klip-worker-log".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                // Keep draining even if our stderr is gone, so the worker never blocks.
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let _ = std::io::stderr().write_all(buf.get(..n).unwrap_or(&[]));
                }
            });
    }
    Ok(child)
}

/// Wait for `child` to exit; kill it after `timeout`. `None`: it had to be killed (or
/// can't be waited for).
fn wait_or_kill(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) if deadline.is_none_or(|d| Instant::now() < d) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Shared between a [`ChildRecorder`] and its reader thread.
#[derive(Default)]
struct Link {
    pending: Mutex<HashMap<u64, SyncSender<Response>>>,
    /// The process's output ended (it exited or closed it).
    closed: AtomicBool,
    /// [`ChildRecorder::finish`] began: an exit from now on is expected.
    finishing: AtomicBool,
}

/// A recorder in a child process, driven over its stdin/stdout.
pub(crate) struct ChildRecorder {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    link: Arc<Link>,
    next_id: AtomicU64,
    pid: u32,
}

impl ChildRecorder {
    fn start(
        exe: &Path,
        settings: &Settings,
        game: Option<&DetectedGame>,
        encoders: &[EncoderChoice],
    ) -> Result<Started, EngineError> {
        let request = StartRequest {
            settings: settings
                .to_value()
                .map_err(|e| EngineError::Config(e.to_string()))?,
            game: game.cloned(),
            encoders: encoders.iter().map(|c| c.element.clone()).collect(),
        };
        let mut child = spawn_worker(exe, &[worker::RECORDER_FLAG.as_ref()], Stdio::piped())
            .map_err(|e| {
                EngineError::Config(format!("could not start the recorder process: {e}"))
            })?;
        let pid = child.id();
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Config("recorder process without pipes".into()));
        };
        let link = Arc::new(Link::default());
        let (start_tx, start_rx) = sync_channel::<StartReply>(1);
        let reader_link = link.clone();
        let reader = std::thread::Builder::new()
            .name("klip-recorder-link".into())
            .spawn(move || read_replies(stdout, &start_tx, &reader_link));
        let fail = |child: &mut Child, msg: String| {
            let _ = child.kill();
            let _ = child.wait();
            EngineError::Config(msg)
        };
        if let Err(e) = reader {
            return Err(fail(
                &mut child,
                format!("no thread for the recorder link: {e}"),
            ));
        }
        if let Err(e) = worker::write_line(&mut stdin, &request) {
            return Err(fail(&mut child, format!("recorder process: {e}")));
        }
        let reply = match start_rx.recv_timeout(START_TIMEOUT) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => {
                return Err(fail(
                    &mut child,
                    "the recorder process did not start in time".into(),
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = wait_or_kill(&mut child, Duration::from_secs(5));
                return Err(EngineError::Config(format!(
                    "the recorder process ended while starting ({})",
                    describe_exit(status)
                )));
            }
        };
        if !reply.ok {
            // It exits by itself after saying why.
            drop(stdin);
            let _ = wait_or_kill(&mut child, Duration::from_secs(10));
            let error: String = reply
                .error
                .unwrap_or_else(|| "the recorder did not start".into())
                .chars()
                .take(500)
                .collect();
            return Err(EngineError::Config(error));
        }
        let attempts = reply
            .attempts
            .into_iter()
            .take(32)
            .map(|a| EncoderAttempt {
                element: a.element,
                error: a.error,
            })
            .collect();
        let clip_dir = reply.clip_dir.unwrap_or_default();
        tracing::info!(pid, "recorder process started");
        Ok(Started {
            handle: RecorderHandle::Child(ChildRecorder {
                child: Mutex::new(child),
                stdin: Mutex::new(Some(stdin)),
                link,
                next_id: AtomicU64::new(1),
                pid,
            }),
            clip_dir,
            fps: reply.fps,
            buffer_ms: reply.buffer_ms,
            status: reply.status,
            attempts,
        })
    }

    fn closed_error() -> CommandError {
        CommandError::new("not_ready", "the recorder process has ended")
    }

    fn call(&self, cmd: &str, args: &Value, timeout: Duration) -> Result<Value, CommandError> {
        if self.link.closed.load(Ordering::Acquire) {
            return Err(Self::closed_error());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = sync_channel(1);
        lock(&self.link.pending).insert(id, tx);
        let forget = || {
            lock(&self.link.pending).remove(&id);
        };
        // The reader may have closed the link between the check and the insert.
        if self.link.closed.load(Ordering::Acquire) {
            forget();
            return Err(Self::closed_error());
        }
        let req = Request {
            id,
            cmd: cmd.into(),
            args: args.clone(),
        };
        let written = match lock(&self.stdin).as_mut() {
            Some(w) => worker::write_line(w, &req).is_ok(),
            None => false,
        };
        if !written {
            forget();
            return Err(Self::closed_error());
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => from_response(r),
            Err(RecvTimeoutError::Timeout) => {
                forget();
                tracing::warn!(cmd, "the recorder process did not answer in time");
                Err(CommandError::new(
                    "failed",
                    "the recorder did not answer in time",
                ))
            }
            Err(RecvTimeoutError::Disconnected) => Err(Self::closed_error()),
        }
    }

    fn ended(&self) -> Option<String> {
        if !self.link.closed.load(Ordering::Acquire) || self.link.finishing.load(Ordering::Acquire)
        {
            return None;
        }
        let status = wait_or_kill(&mut lock(&self.child), Duration::from_secs(2));
        Some(describe_exit(status))
    }

    fn finish(&self) -> Stopped {
        self.link.finishing.store(true, Ordering::Release);
        let reply = self.call(worker::FINISH, &Value::Null, WRITE_TIMEOUT);
        // Closing its input lets the process exit once saves in flight are written.
        drop(lock(&self.stdin).take());
        let status = wait_or_kill(&mut lock(&self.child), EXIT_TIMEOUT);
        match &reply {
            Ok(_) if status.is_some_and(|s| s.success()) => {
                tracing::info!(pid = self.pid, "recorder process exited");
            }
            Ok(_) => {
                tracing::warn!(pid = self.pid, exit = %describe_exit(status), "recorder process did not exit cleanly")
            }
            Err(e) => {
                tracing::warn!(pid = self.pid, error = %e.message, exit = %describe_exit(status), "recorder process did not finish")
            }
        }
        // Without an answer the recording may not have been finalised.
        let unclean = reply.is_err();
        let summary = reply
            .ok()
            .and_then(|v| v.get("summary").cloned())
            .filter(|v| !v.is_null());
        Stopped { summary, unclean }
    }
}

impl Drop for ChildRecorder {
    fn drop(&mut self) {
        // Not finished (the engine is going away): end of input makes the process
        // finalise a running recording and exit by itself.
        drop(lock(&self.stdin).take());
    }
}

fn describe_exit(status: Option<ExitStatus>) -> String {
    match status {
        Some(s) => s.to_string(),
        None => "killed".into(),
    }
}

fn from_response(r: Response) -> Result<Value, CommandError> {
    if r.ok {
        return Ok(r.result.unwrap_or(Value::Null));
    }
    let (code, message) = r
        .error
        .map(|e| (e.code, e.message))
        .unwrap_or_else(|| ("failed".into(), "failed".into()));
    let code: &'static str = match code.as_str() {
        "bad_args" => "bad_args",
        "not_ready" => "not_ready",
        "busy" => "busy",
        "unknown_command" => "unknown_command",
        "internal" => "internal",
        _ => "failed",
    };
    Err(CommandError::new(
        code,
        message.chars().take(500).collect::<String>(),
    ))
}

/// Read the recorder process's output: the start reply, then responses, until it ends.
fn read_replies(stdout: impl Read, start: &SyncSender<StartReply>, link: &Link) {
    let mut r = BufReader::new(stdout);
    let mut started = false;
    loop {
        let line = match klip_control::read_line_capped(&mut r, MAX_REPLY_LINE) {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "recorder process output");
                break;
            }
        };
        if let Ok(resp) = serde_json::from_slice::<Response>(&line) {
            if let Some(tx) = lock(&link.pending).remove(&resp.id) {
                let _ = tx.send(resp);
            }
        } else if !started && let Ok(s) = serde_json::from_slice::<StartLine>(&line) {
            started = true;
            let _ = start.send(s.start);
        } else {
            tracing::debug!("unexpected output from the recorder process");
        }
    }
    link.closed.store(true, Ordering::Release);
    // Waiters see the link close.
    lock(&link.pending).clear();
}

#[cfg(test)]
mod tests {
    use klip_control::ErrorBody;

    use super::*;

    #[test]
    fn replies_are_routed_and_junk_is_ignored() {
        let link = Link::default();
        let (tx, rx) = sync_channel(1);
        lock(&link.pending).insert(7, tx);
        let (start_tx, start_rx) = sync_channel(1);
        let mut input = [
            r#"a stray print"#,
            r#"{"start":{"ok":true,"fps":60}}"#,
            r#"{"start":{"ok":false}}"#,
            r#"{"id":9,"ok":true}"#,
            r#"{"id":7,"ok":false,"error":{"code":"busy","message":"m"}}"#,
        ]
        .join("\n")
        .into_bytes();
        input.extend_from_slice(&[b'\n', 0xFF, 0xFE, b'\n']);
        read_replies(&input[..], &start_tx, &link);
        let s = start_rx.try_recv().unwrap();
        assert!(s.ok);
        assert_eq!(s.fps, 60);
        assert!(start_rx.try_recv().is_err(), "one start reply only");
        let e = from_response(rx.try_recv().unwrap()).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), ("busy", "m"));
        assert!(link.closed.load(Ordering::Acquire));
        assert!(lock(&link.pending).is_empty());
    }

    #[test]
    fn a_line_too_long_closes_the_link() {
        let link = Link::default();
        let (tx, rx) = sync_channel(1);
        lock(&link.pending).insert(1, tx);
        let (start_tx, _start_rx) = sync_channel(1);
        let input = vec![b'x'; MAX_REPLY_LINE + 10];
        read_replies(&input[..], &start_tx, &link);
        assert!(link.closed.load(Ordering::Acquire));
        assert!(rx.try_recv().is_err(), "waiter released");
    }

    #[test]
    fn worker_errors_are_mapped_and_capped() {
        let r = Response {
            id: 1,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: "weird".into(),
                message: "x".repeat(10_000),
            }),
        };
        let e = from_response(r).unwrap_err();
        assert_eq!(e.code, "failed");
        assert_eq!(e.message.chars().count(), 500);
        let r = Response {
            id: 1,
            ok: true,
            result: None,
            error: None,
        };
        assert_eq!(from_response(r).unwrap(), Value::Null);
    }

    #[test]
    fn in_process_without_workers() {
        // This test binary never calls worker_main.
        assert_eq!(Isolation::auto(), Isolation::InProcess);
    }
}
