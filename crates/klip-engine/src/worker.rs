//! The recorder process (ADR 0016): each recorder runs in a copy of the app's executable
//! that lives as long as the game, so the GPU encoder and driver memory it loads goes
//! away with it instead of staying in the engine for good.
//!
//! The host binary calls [`worker_main`] at the start of `main` (after
//! `klip_probe::worker_main` and logging set-up). Started as `<exe> --klip-recorder` it
//! is one recorder. Protocol, JSON lines over stdin/stdout: the engine sends a [`StartRequest`], the worker answers one [`StartLine`], then
//! serves `klip_control::Request`s (`clip.save`, `session.*`, `bookmark.add`,
//! `recorder.status`, [`STATUS`], [`FINISH`]) with `klip_control::Response`s, each on its
//! own thread so a status read never waits for a save. [`FINISH`] stops capture
//! (finalising a running recording) and answers the summary; end of input does the same
//! without an answer (the engine went away). The worker exits once requests in flight
//! are answered.
//!
//! Logs go to stderr, which the engine passes on to its own.

use std::io::{BufRead, BufReader, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use klip_control::{CommandError, ErrorBody, Handler, MAX_LINE, Request, Response, commands};
use klip_recorder::Recorder;
use klip_settings::Settings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config;
use crate::games::DetectedGame;

/// First argument that turns the host binary into a recorder process.
pub const RECORDER_FLAG: &str = "--klip-recorder";

/// Worker command: recorder status plus whether a recording runs.
pub(crate) const STATUS: &str = "worker.status";
/// Worker command: stop capturing (finalise a running recording); answers
/// `{"summary": …|null}`.
pub(crate) const FINISH: &str = "worker.finish";

/// Longest start request (the settings file is capped at 1 MiB).
pub(crate) const MAX_START_LINE: usize = 2 * 1024 * 1024;
/// Requests handled at once; more are answered `busy`.
const MAX_JOBS: usize = 16;
/// How long a finishing worker waits for requests in flight (a clip being written).
const DRAIN: Duration = Duration::from_secs(120);

/// Whether this process calls [`worker_main`], i.e. its executable can host workers.
static HOSTS_WORKERS: AtomicBool = AtomicBool::new(false);

pub(crate) fn hosts_workers() -> bool {
    HOSTS_WORKERS.load(Ordering::Relaxed)
}

/// What a recorder process records: the engine's settings, the game and the encoders it
/// picked (best first; the engine owns the probe cache).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartRequest {
    pub settings: Value,
    pub game: Option<DetectedGame>,
    pub encoders: Vec<String>,
}

/// One encoder tried at start.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Attempt {
    pub element: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// The recorder process's first line: running, or why not.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartReply {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default)]
    pub status: Value,
    #[serde(default)]
    pub clip_dir: Option<PathBuf>,
    #[serde(default)]
    pub fps: u32,
    #[serde(default)]
    pub buffer_ms: i64,
}

/// Wrapper that tells the start reply apart from responses (and stray output).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartLine {
    pub start: StartReply,
}

pub(crate) fn write_line(w: &mut impl Write, v: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// If this process was started as a worker, do its job and return the exit code;
/// otherwise `None`. Call early in `main`, before anything touches GStreamer, the GPU or
/// the portals (a recorder sets up DPI awareness and the portal app id itself, once its
/// start request checked out).
pub fn worker_main() -> Option<i32> {
    HOSTS_WORKERS.store(true, Ordering::Relaxed);
    if std::env::args_os().nth(1)? != RECORDER_FLAG {
        return None;
    }
    Some(guarded("recorder", run_recorder))
}

fn guarded(what: &str, f: impl FnOnce() -> Result<(), String>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "{what} worker");
            1
        }
        Err(_) => {
            tracing::error!("{what} worker panicked");
            2
        }
    }
}

fn platform_setup() {
    // Window sizes in physical pixels, as the engine sees them.
    klip_platform::enable_dpi_awareness();
    // Linux: the portals remember screen-cast choices per app id; name ourselves first.
    if cfg!(target_os = "linux")
        && let Err(e) = klip_platform::register_app_id(klip_platform::APP_ID)
    {
        tracing::warn!(error = %e, "could not register the app id with the portals");
    }
}

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, v: &impl Serialize) {
    let mut w = out.lock().unwrap_or_else(PoisonError::into_inner);
    // The engine went away: nothing to tell.
    let _ = write_line(&mut *w, v);
}

fn start(req: &StartRequest) -> Result<(Recorder, StartReply), String> {
    let (settings, issues) = Settings::from_json_lenient(&req.settings.to_string());
    for i in &issues {
        tracing::warn!(issue = %i, "settings from the engine");
    }
    klip_probe::init().map_err(|e| format!("GStreamer: {e}"))?;
    let game = req.game.as_ref();
    let encoders = config::encoders_named(&settings, game, &req.encoders);
    if encoders.is_empty() {
        return Err("no encoder".into());
    }
    platform_setup();
    let plan = config::plan_with(&settings, game, encoders).map_err(|e| e.to_string())?;
    let buffer_ms = plan
        .config
        .buffer
        .checked_div(klip_media::NANOS_PER_MILLI)
        .unwrap_or(0);
    let rec = Recorder::start(plan.config).map_err(|e| e.to_string())?;
    let reply = StartReply {
        ok: true,
        error: None,
        attempts: rec
            .encoder_attempts()
            .iter()
            .map(|a| Attempt {
                element: a.element.clone(),
                error: a.error.clone(),
            })
            .collect(),
        status: rec.status(),
        clip_dir: Some(plan.clip_dir),
        fps: plan.fps,
        buffer_ms,
    };
    Ok((rec, reply))
}

fn run_recorder() -> Result<(), String> {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut input = BufReader::new(std::io::stdin().lock());
    let line = klip_control::read_line_capped(&mut input, MAX_START_LINE)
        .map_err(|e| format!("reading the start request: {e}"))?
        .ok_or("no start request")?;
    let req: StartRequest =
        serde_json::from_slice(&line).map_err(|e| format!("bad start request: {e}"))?;
    let rec = match start(&req) {
        Ok((rec, reply)) => {
            send(&out, &StartLine { start: reply });
            Arc::new(rec)
        }
        Err(error) => {
            send(
                &out,
                &StartLine {
                    start: StartReply {
                        ok: false,
                        error: Some(error),
                        ..StartReply::default()
                    },
                },
            );
            return Ok(());
        }
    };
    serve(&rec, &mut input, &out);
    Ok(())
}

/// Answer requests until [`FINISH`] or the end of input, then stop capturing and wait
/// for requests in flight.
fn serve(rec: &Arc<Recorder>, input: &mut impl BufRead, out: &Out) {
    let jobs = Arc::new(AtomicUsize::new(0));
    let mut finished = false;
    loop {
        let line = match klip_control::read_line_capped(input, MAX_LINE) {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "recorder worker input");
                break;
            }
        };
        let req: Request = match serde_json::from_slice(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "bad request to the recorder worker");
                continue;
            }
        };
        if req.cmd == FINISH {
            let summary = rec.stop_capture().map(|s| s.to_json());
            send(out, &ok_response(req.id, json!({ "summary": summary })));
            finished = true;
            break;
        }
        if jobs.load(Ordering::Relaxed) >= MAX_JOBS {
            let e = CommandError::new("busy", "the recorder is busy");
            send(out, &err_response(req.id, &e));
            continue;
        }
        jobs.fetch_add(1, Ordering::Relaxed);
        let id = req.id;
        let (rec, out2, jobs2) = (rec.clone(), out.clone(), jobs.clone());
        let spawned = std::thread::Builder::new()
            .name("klip-recorder-job".into())
            .spawn(move || {
                let res = catch_unwind(AssertUnwindSafe(|| run_request(&rec, &req)))
                    .unwrap_or_else(|_| Err(CommandError::new("internal", "the command panicked")));
                let resp = match res {
                    Ok(v) => ok_response(id, v),
                    Err(e) => err_response(id, &e),
                };
                send(&out2, &resp);
                jobs2.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            jobs.fetch_sub(1, Ordering::Relaxed);
            let e = CommandError::new("failed", "no thread for the command");
            send(out, &err_response(id, &e));
        }
    }
    if !finished {
        // The engine closed the pipe or died: finalise a running recording all the same.
        if rec.stop_capture().is_some() {
            tracing::info!("recording finalised after the engine went away");
        }
    }
    let deadline = Instant::now().checked_add(DRAIN);
    while jobs.load(Ordering::Relaxed) > 0 && deadline.is_some_and(|d| Instant::now() < d) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_request(rec: &Recorder, req: &Request) -> Result<Value, CommandError> {
    match req.cmd.as_str() {
        STATUS => Ok(json!({
            "status": rec.status(),
            "session_running": rec.session_running(),
        })),
        // The engine stops the recorder with FINISH; this one would only set a flag.
        commands::RECORDER_STOP => Err(CommandError::unknown(&req.cmd)),
        _ => rec.handle(&req.cmd, &req.args),
    }
}

fn ok_response(id: u64, result: Value) -> Response {
    Response {
        id,
        ok: true,
        result: Some(result),
        error: None,
    }
}

fn err_response(id: u64, e: &CommandError) -> Response {
    Response {
        id,
        ok: false,
        result: None,
        error: Some(ErrorBody {
            code: e.code.into(),
            message: e.message.clone(),
        }),
    }
}
