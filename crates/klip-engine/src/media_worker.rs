//! Library thumbnails and exports in a child process (ADR 0015, ADR 0017).
//!
//! The clip folder can hold any file. A demuxer, decoder or encoder that crashes or
//! hangs on one must not take the app (and the replay buffer of a running game) down
//! with it, so posters, hover-scrub strips and exports are made by a copy of the app
//! started with [`WORKER_FLAG`]: one job per JSON line on its stdin, replies per line
//! on its stdout. The host binary calls [`worker_main`] first thing in `main`.
//!
//! Thumbnail jobs ([`ThumbJob`]) run one after the other and answer with one
//! [`ThumbReply`]. Export jobs ([`ExportJob`]) run on their own thread, send
//! [`ExportReply`] progress lines and one final reply, and can be stopped with a
//! [`CancelJob`] line. A job that is silent longer than its timeout gets the worker
//! killed; the next job starts a new one. An idle worker is stopped, so it holds no
//! memory between batches. The worker's output is untrusted: lines are size-capped
//! and parsed strictly.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use klip_export::{ExportSummary, Progress};
use serde::{Deserialize, Serialize};

/// First argument that turns the host binary into a media worker.
pub const WORKER_FLAG: &str = "--klip-media-worker";
/// Longest line read from either side.
const MAX_LINE: u64 = 64 * 1024;
/// Longest line an export job may be (the plan carries text layers).
const MAX_JOB_LINE: u64 = 1024 * 1024;
/// A worker unused this long is stopped.
pub const IDLE_STOP: Duration = Duration::from_secs(20);
/// Where on the clip the poster is taken: this long before its end (the hotkey moment).
pub const POSTER_BACK: Duration = Duration::from_millis(500);
/// How long the host waits for an export to say something (a progress line counts).
pub const EXPORT_INACTIVITY: Duration = Duration::from_secs(120);
/// How often the host looks at the cancel flag while waiting.
const CANCEL_POLL: Duration = Duration::from_millis(200);
/// How long a worker waits for its export threads when stdin closes.
const WORKER_DRAIN: Duration = Duration::from_secs(10);

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

/// Export a clip with a plan (ADR 0017).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportJob {
    pub id: u64,
    pub clip: PathBuf,
    /// Where the MP4 goes (must not exist).
    pub out: PathBuf,
    /// What to render (`klip_edit::compose` of the project).
    pub composition: klip_edit::Composition,
    /// Files of the composition's other sources, as the engine resolved them.
    #[serde(default)]
    pub media: Vec<(klip_edit::Source, PathBuf)>,
    /// A [`klip_export::Preset`] id.
    pub preset: String,
    /// Encoder element names, best first (`klip_export::Encoders::names`).
    pub encoders: Vec<String>,
    pub discord_limit_bytes: u64,
    /// Test knob, see `klip_export::ExportRequest::pretend_overshoot`.
    #[serde(default)]
    pub pretend_overshoot: f64,
}

/// Stop an export.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelJob {
    pub cancel: u64,
}

/// What the worker reads: one of the jobs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Job {
    Cancel(CancelJob),
    Thumb(ThumbJob),
    Export(Box<ExportJob>),
}

/// A progress line (`progress` set) or the final reply (`ok` set) of an export.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportReply {
    pub id: u64,
    #[serde(default)]
    pub progress: Option<Progress>,
    #[serde(default)]
    pub ok: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub summary: Option<ExportSummary>,
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

/// Do an export in this process, reporting progress and honouring `cancel`.
pub fn run_export(
    job: &ExportJob,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<ExportSummary, String> {
    let preset = klip_export::Preset::parse(&job.preset)
        .ok_or_else(|| format!("unknown preset {:?}", job.preset))?;
    // Media Foundation must start before any NVENC session in this process (ADR 0009).
    klip_probe::init().map_err(|e| e.to_string())?;
    let encoders = klip_export::Encoders::from_names(&job.encoders);
    let req = klip_export::ExportRequest {
        source: &job.clip,
        composition: &job.composition,
        media: &job.media,
        preset,
        out: &job.out,
        encoders: &encoders,
        discord_limit_bytes: job.discord_limit_bytes,
        pretend_overshoot: job.pretend_overshoot,
    };
    klip_export::export(&req, progress, cancel).map_err(|e| e.to_string())
}

fn run_export_caught(
    job: &ExportJob,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<ExportSummary, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_export(job, progress, cancel)
    }))
    .unwrap_or_else(|_| Err("export job panicked".into()))
}

/// One line, at most `max` bytes; `None` at the end of the stream. A longer line is
/// cut (and won't parse).
fn read_line<R: BufRead>(r: &mut R, max: u64) -> Option<String> {
    let mut buf = Vec::new();
    match r.by_ref().take(max).read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(String::from_utf8_lossy(&buf).into_owned()),
    }
}

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, reply: &impl Serialize) -> bool {
    let text = serde_json::to_string(reply).unwrap_or_else(|_| "{}".into());
    let mut o = out.lock().unwrap_or_else(PoisonError::into_inner);
    writeln!(o, "{text}").and_then(|()| o.flush()).is_ok()
}

/// If this process was started as a media worker, serve jobs until stdin closes and
/// return the exit code; otherwise `None`. Call at the very start of `main`.
pub fn worker_main() -> Option<i32> {
    if std::env::args().nth(1).as_deref() != Some(WORKER_FLAG) {
        return None;
    }
    // Key frames decode faster in software than a GPU decoder starts (ADR 0016); an
    // export decodes in software too (the encoder is the hardware part).
    let _ = klip_capture::thumbnail::prefer_software_decoders();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let cancels: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut exports: Vec<std::thread::JoinHandle<()>> = Vec::new();
    while let Some(line) = read_line(&mut input, MAX_JOB_LINE) {
        match serde_json::from_str::<Job>(line.trim()) {
            Ok(Job::Thumb(job)) => {
                let r = run_job_caught(&job);
                let reply = ThumbReply {
                    id: job.id,
                    ok: r.is_ok(),
                    error: r.err(),
                };
                if !send(&out, &reply) {
                    break;
                }
            }
            Ok(Job::Cancel(c)) => {
                if let Some(flag) = cancels
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&c.cancel)
                {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            Ok(Job::Export(job)) => {
                let job_id = job.id;
                let cancel = Arc::new(AtomicBool::new(false));
                cancels
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(job.id, cancel.clone());
                let out2 = out.clone();
                let cancels2 = cancels.clone();
                let t = std::thread::Builder::new()
                    .name(format!("klip-export-{}", job.id))
                    .spawn(move || {
                        let id = job.id;
                        let mut progress = |p: Progress| {
                            send(
                                &out2,
                                &ExportReply {
                                    id,
                                    progress: Some(p),
                                    ok: None,
                                    error: None,
                                    summary: None,
                                },
                            );
                        };
                        let r = run_export_caught(&job, &mut progress, &cancel);
                        cancels2
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .remove(&id);
                        let reply = match r {
                            Ok(summary) => ExportReply {
                                id,
                                progress: None,
                                ok: Some(true),
                                error: None,
                                summary: Some(summary),
                            },
                            Err(e) => ExportReply {
                                id,
                                progress: None,
                                ok: Some(false),
                                error: Some(e),
                                summary: None,
                            },
                        };
                        send(&out2, &reply);
                    });
                match t {
                    Ok(handle) => exports.push(handle),
                    Err(e) => {
                        send(
                            &out,
                            &ExportReply {
                                id: job_id,
                                progress: None,
                                ok: Some(false),
                                error: Some(format!("no thread: {e}")),
                                summary: None,
                            },
                        );
                    }
                }
            }
            Err(e) => {
                let reply = ThumbReply {
                    id: 0,
                    ok: false,
                    error: Some(format!("bad job: {e}")),
                };
                if !send(&out, &reply) {
                    break;
                }
            }
        }
        exports.retain(|h| !h.is_finished());
    }
    // The host is gone: stop exports and give them a moment to clean up.
    for flag in cancels
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
    {
        flag.store(true, Ordering::Relaxed);
    }
    let deadline = Instant::now().checked_add(WORKER_DRAIN);
    for h in exports {
        while !h.is_finished() && deadline.is_some_and(|d| Instant::now() < d) {
            std::thread::sleep(Duration::from_millis(20));
        }
        if h.is_finished() {
            let _ = h.join();
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

/// Runs thumbnail and export jobs: in a child process (the app) or in this process
/// (tests).
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

    fn ensure(&mut self) -> Result<&mut Running, String> {
        let Some(exe) = &self.exe else {
            return Err(format!("{WORKER_FAILED} is in-process"));
        };
        if self.running.is_none() {
            self.running = Some(spawn(exe)?);
        }
        self.running
            .as_mut()
            .ok_or_else(|| format!("{WORKER_FAILED} is missing"))
    }

    fn after(&mut self, r: &Result<impl Sized, String>) {
        if r.as_ref().is_err_and(|e| e.starts_with(WORKER_FAILED)) {
            // Timed out, died or talked nonsense: start afresh next time.
            self.running = None;
        }
        self.last_used = Instant::now();
    }

    /// Run one thumbnail job, waiting at most `timeout` for it.
    pub fn run(&mut self, job: &ThumbJob, timeout: Duration) -> Result<(), String> {
        if self.exe.is_none() {
            return run_job_caught(job);
        }
        self.last_used = Instant::now();
        let r = self.ensure().and_then(|w| ask(w, job, timeout));
        self.after(&r);
        r
    }

    /// Run one export, reporting progress; `cancel` stops it (the worker answers with
    /// an error). The worker is replaced when it is silent for `inactivity`.
    pub fn export(
        &mut self,
        job: &ExportJob,
        progress: &mut dyn FnMut(Progress),
        cancel: &AtomicBool,
        inactivity: Duration,
    ) -> Result<ExportSummary, String> {
        if self.exe.is_none() {
            return run_export_caught(job, progress, cancel);
        }
        self.last_used = Instant::now();
        let r = self
            .ensure()
            .and_then(|w| ask_export(w, job, progress, cancel, inactivity));
        self.after(&r);
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
    let (tx, rx) = sync_channel::<String>(64);
    let reader = std::thread::Builder::new()
        .name("klip-media-worker-out".into())
        .spawn(move || {
            let mut out = BufReader::new(stdout);
            while let Some(line) = read_line(&mut out, MAX_LINE) {
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

fn write_job(w: &mut Running, job: &impl Serialize) -> Result<(), String> {
    let line = serde_json::to_string(job).map_err(|e| e.to_string())?;
    if line.len() as u64 >= MAX_JOB_LINE {
        return Err("the job is too big".into());
    }
    writeln!(w.stdin, "{line}")
        .and_then(|()| w.stdin.flush())
        .map_err(|e| format!("{WORKER_FAILED} is gone: {e}"))
}

fn cap(e: Option<String>) -> String {
    e.unwrap_or_else(|| "failed".into())
        .chars()
        .take(300)
        .collect()
}

fn ask(w: &mut Running, job: &ThumbJob, timeout: Duration) -> Result<(), String> {
    write_job(w, job)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match w.replies.recv_timeout(left) {
            Ok(line) => match serde_json::from_str::<ThumbReply>(line.trim()) {
                Ok(r) if r.id == job.id => {
                    return if r.ok { Ok(()) } else { Err(cap(r.error)) };
                }
                // A stale reply (an earlier job that timed out can't be one: the worker
                // was replaced), an export's line, or junk: keep waiting.
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

fn ask_export(
    w: &mut Running,
    job: &ExportJob,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
    inactivity: Duration,
) -> Result<ExportSummary, String> {
    write_job(w, job)?;
    let mut last_heard = Instant::now();
    let mut cancel_sent = false;
    loop {
        if cancel.load(Ordering::Relaxed) && !cancel_sent {
            cancel_sent = true;
            write_job(w, &CancelJob { cancel: job.id })?;
        }
        if last_heard.elapsed() >= inactivity {
            return Err(format!("{WORKER_FAILED} timed out"));
        }
        match w.replies.recv_timeout(CANCEL_POLL) {
            Ok(line) => {
                let Ok(r) = serde_json::from_str::<ExportReply>(line.trim()) else {
                    continue;
                };
                if r.id != job.id {
                    continue;
                }
                last_heard = Instant::now();
                if let Some(p) = r.progress {
                    progress(p);
                }
                match (r.ok, r.summary) {
                    (Some(true), Some(s)) => return Ok(s),
                    (Some(true), None) => {
                        return Err(format!("{WORKER_FAILED} answered without a summary"));
                    }
                    (Some(false), _) => return Err(cap(r.error)),
                    (None, _) => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(format!("{WORKER_FAILED} exited"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_lines_parse_by_shape() {
        let thumb = r#"{"id":3,"clip":"a.mp4","poster":null,"poster_width":320,"strip":null,"tiles":16,"tile_width":240}"#;
        assert!(matches!(serde_json::from_str::<Job>(thumb), Ok(Job::Thumb(j)) if j.id == 3));
        let cancel = r#"{"cancel":7}"#;
        assert!(matches!(serde_json::from_str::<Job>(cancel), Ok(Job::Cancel(c)) if c.cancel == 7));
        let export = r#"{"id":9,"clip":"a.mp4","out":"b.mp4","composition":{},"preset":"discord","encoders":["nvd3d11h264enc"],"discord_limit_bytes":10000000}"#;
        assert!(
            matches!(serde_json::from_str::<Job>(export), Ok(Job::Export(j)) if j.id == 9 && j.pretend_overshoot == 0.0)
        );
        assert!(serde_json::from_str::<Job>(r#"{"id":1,"nonsense":true}"#).is_err());
        assert!(serde_json::from_str::<Job>("[]").is_err());
    }

    #[test]
    fn export_replies_round_trip() {
        let p = ExportReply {
            id: 1,
            progress: Some(Progress {
                pass: 1,
                frames_done: 10,
                frames_total: 100,
            }),
            ok: None,
            error: None,
            summary: None,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<ExportReply>(&s).unwrap(), p);
        // A thumb reply is not an export reply and the other way round: ids keep them apart anyway.
        assert!(serde_json::from_str::<ThumbReply>(&s).is_err());
    }

    #[test]
    fn in_process_export_reports_plan_errors() {
        let dir = tempfile::tempdir().unwrap();
        let job = ExportJob {
            id: 1,
            clip: dir.path().join("missing.mp4"),
            out: dir.path().join("out.mp4"),
            composition: klip_edit::Composition::default(),
            media: vec![],
            preset: "youtube".into(),
            encoders: vec![],
            discord_limit_bytes: 10_000_000,
            pretend_overshoot: 0.0,
        };
        let mut w = MediaWorker::in_process();
        let e = w
            .export(
                &job,
                &mut |_| {},
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap_err();
        assert!(e.contains("cannot be exported"), "{e}");
        let bad_preset = ExportJob {
            preset: "nope".into(),
            ..job
        };
        let e = w
            .export(
                &bad_preset,
                &mut |_| {},
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap_err();
        assert!(e.contains("unknown preset"), "{e}");
        assert!(!dir.path().join("out.mp4").exists());
    }
}
