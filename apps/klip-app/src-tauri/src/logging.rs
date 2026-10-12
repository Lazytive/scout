//! Log files: every copy of Scout (the app, the recorder, media and encoder-probe
//! workers) writes to `<local data>/Scout/logs/` (Windows `%LOCALAPPDATA%\Scout\logs`),
//! one file per process kind and day, the last 7 kept, with the user's home folder
//! written as `~` (AGENTS rule 4). The app and the recorder also log to stderr.
//!
//! The `full-logs` feature is a tester build (the user's AMD tester, 2026-10-12), never
//! a release: Scout at debug, and GStreamer's own log (warnings; AMD AMF at debug, Media
//! Foundation and Direct3D at info) to `gstreamer-<kind>-<pid>.log`.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

/// A tester build with everything logged.
pub const FULL: bool = cfg!(feature = "full-logs");
/// Log files older than this are removed at start.
const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

/// Where the logs go.
pub fn dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Scout").join("logs"))
}

/// Which copy of the exe this is, from its first argument.
pub fn kind() -> &'static str {
    let first = std::env::args_os().nth(1).unwrap_or_default();
    if first == klip_engine::RECORDER_FLAG {
        "recorder"
    } else if first == klip_engine::media_worker::WORKER_FLAG {
        "media"
    } else if first == klip_probe::isolated::WORKER_FLAG {
        "probe"
    } else {
        "app"
    }
}

/// The environment GStreamer reads for its own log (full-logs builds); set before any
/// thread starts (`klip_platform::set_env_before_threads`).
pub fn gstreamer_env(kind: &str) -> Vec<(&'static str, OsString)> {
    let Some(dir) = dir().filter(|d| FULL && std::fs::create_dir_all(d).is_ok()) else {
        return Vec::new();
    };
    let file = dir.join(format!("gstreamer-{kind}-{}.log", std::process::id()));
    vec![
        (
            "GST_DEBUG",
            "2,*amf*:5,amfcodec:5,mf*:4,d3d11*:4,d3d12*:4,nvcodec:4,qsv*:4".into(),
        ),
        ("GST_DEBUG_FILE", file.into_os_string()),
        ("GST_DEBUG_NO_COLOR", "1".into()),
    ]
}

/// Remove log files older than [`KEEP`] (the app only; workers start often).
pub fn prune() {
    let Some(dir) = dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP);
        if old && e.path().extension().is_some_and(|x| x == "log") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The user's home folder as it may appear in a log line (plain, `/`, Debug-escaped).
fn home_forms() -> Vec<(String, &'static str)> {
    let Some(home) = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    if home.len() < 4 {
        return Vec::new();
    }
    vec![
        (home.replace('\\', "\\\\"), "~"),
        (home.replace('\\', "/"), "~"),
        (home, "~"),
    ]
}

/// The day's file, written one whole event at a time with the home folder hidden.
#[derive(Clone)]
struct FileLog {
    file: Arc<Mutex<RollingFileAppender>>,
    home: Arc<Vec<(String, &'static str)>>,
}

struct EventWriter {
    log: FileLog,
    buf: Vec<u8>,
}

impl Write for EventWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        let mut text = String::from_utf8_lossy(&self.buf).into_owned();
        for (from, to) in self.log.home.iter() {
            text = text.replace(from.as_str(), to);
        }
        let mut f = self.log.file.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = f.write_all(text.as_bytes());
    }
}

impl<'a> MakeWriter<'a> for FileLog {
    type Writer = EventWriter;

    fn make_writer(&'a self) -> EventWriter {
        EventWriter {
            log: self.clone(),
            buf: Vec::new(),
        }
    }
}

/// Start logging for this copy of the exe; panics are logged too.
pub fn init(kind: &str) {
    let default = if FULL { "info,klip=debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let file = dir()
        .filter(|d| std::fs::create_dir_all(d).is_ok())
        .and_then(|d| {
            RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix(format!("scout-{kind}"))
                .filename_suffix("log")
                .max_log_files(7)
                .build(d)
                .ok()
        })
        .map(|f| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(FileLog {
                    file: Arc::new(Mutex::new(f)),
                    home: Arc::new(home_forms()),
                })
        });
    // Workers' stderr goes nowhere (or is a pipe nobody reads): file only.
    let stderr = matches!(kind, "app" | "recorder")
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file)
        .with(stderr)
        .try_init();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "panic");
        default_hook(info);
    }));
}
