//! Export commands (M8.4): `export.start {path, preset}` renders a clip's project with a
//! preset into `<name> - <Preset>.mp4` next to the clip (never overwriting), on its own
//! thread through the media worker; `export.cancel {id}`; `export.status`. One export
//! runs at a time (`busy` otherwise). Progress comes as `export_progress` events (at
//! most a few per second), the end as one `export_done`; a finished export is indexed
//! by the library and gets its thumbnails. The editor's preview proxy is the same job
//! with the `proxy` preset, written to `.klip/proxies/`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use klip_control::CommandError;
use klip_edit::{Project, Render};
use klip_export::{Preset, Progress};
use klip_library::{ClipInfo, Library};
use serde_json::{Value, json};

use super::editor::source_of;
use super::library::str_arg;
use super::{Inner, lock};
use crate::config;
use crate::events::Event;
use crate::media_worker::{EXPORT_INACTIVITY, ExportJob, MediaWorker};

/// Progress events are at most this close together (the bus drops when flooded).
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// What an export job makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExportKind {
    /// A user export with a preset, next to the clip.
    Preset(Preset),
    /// The editor's H.264 preview proxy, in `.klip/proxies/`.
    Proxy,
}

impl ExportKind {
    fn preset(self) -> Preset {
        match self {
            ExportKind::Preset(p) => p,
            ExportKind::Proxy => Preset::Proxy,
        }
    }
}

/// The export that runs now.
struct Active {
    id: u64,
    key: String,
    out: PathBuf,
    preset: &'static str,
    cancel: Arc<AtomicBool>,
    progress: Progress,
    started: Instant,
}

/// Export state behind `Inner::exports`.
#[derive(Default)]
pub(super) struct ExportState {
    active: Option<Active>,
    /// The last finished export (what `export_done` said).
    last: Option<Value>,
    next_id: u64,
}

impl Inner {
    /// The id of a running export of `key` with `preset`, if any.
    pub(super) fn export_running_for(&self, key: &str, preset: &str) -> Option<u64> {
        let st = lock(&self.exports);
        st.active
            .as_ref()
            .filter(|a| a.key == key && a.preset == preset)
            .map(|a| a.id)
    }

    /// Stop a running export (engine shutdown).
    pub(super) fn cancel_export(&self) {
        if let Some(a) = &lock(&self.exports).active {
            a.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn export_start(&self, args: &Value) -> Result<Value, CommandError> {
        let preset_id = str_arg(args, "preset")?;
        let preset = Preset::ALL
            .into_iter()
            .find(|p| p.id() == preset_id)
            .ok_or_else(|| CommandError::bad_args("unknown preset"))?;
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let info = self.clip_info(&lib, &path, &key)?;
        let (id, out) = self.start_export(ExportKind::Preset(preset), &lib, path, key, &info)?;
        Ok(json!({ "id": id, "out": out.to_string_lossy(), "preset": preset.id() }))
    }

    pub(super) fn export_cancel(&self, args: &Value) -> Result<Value, CommandError> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| CommandError::bad_args("id must be a number"))?;
        let st = lock(&self.exports);
        match &st.active {
            Some(a) if a.id == id => {
                a.cancel.store(true, Ordering::Relaxed);
                Ok(json!({ "id": id }))
            }
            _ => Err(CommandError::new("not_found", "no such export")),
        }
    }

    pub(super) fn export_status(&self, _args: &Value) -> Result<Value, CommandError> {
        let st = lock(&self.exports);
        let active = st.active.as_ref().map(|a| {
            json!({
                "id": a.id,
                "key": a.key,
                "out": a.out.to_string_lossy(),
                "preset": a.preset,
                "pass": a.progress.pass,
                "frames_done": a.progress.frames_done,
                "frames_total": a.progress.frames_total,
                "elapsed_ms": a.started.elapsed().as_millis() as u64,
            })
        });
        Ok(json!({ "active": active, "last": st.last }))
    }

    /// Start a job for `key`: the project rendered with a preset next to the clip, or
    /// the proxy. Returns the job id and where the file will be.
    pub(super) fn start_export(
        &self,
        kind: ExportKind,
        lib: &Library,
        clip: PathBuf,
        key: String,
        info: &ClipInfo,
    ) -> Result<(u64, PathBuf), CommandError> {
        if lock(&self.exports).active.is_some() {
            return Err(CommandError::new("busy", "an export is already running"));
        }
        let source = source_of(info);
        let (render, out) = match kind {
            ExportKind::Preset(p) => {
                let (project, _, _) = self.load_project(lib, &key, &source)?;
                let out = lib
                    .unique_sibling(&clip, p.name())
                    .map_err(|e| CommandError::new("failed", e.to_string()))?;
                (Render::new(&project, &source), out)
            }
            ExportKind::Proxy => {
                let out = lib
                    .proxy_path(&key)
                    .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
                (Render::new(&Project::new(source.duration_ms), &source), out)
            }
        };
        render.validate().map_err(CommandError::bad_args)?;
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| CommandError::new("failed", e.to_string()))?;
        }
        let settings = self.settings();
        let encoders = {
            let mut c = lock(&self.cache);
            let r = config::export_encoders(&settings, &mut c.0);
            config::save_encoder_cache(&c.0, c.1.as_deref());
            r.map_err(|e| CommandError::new("failed", e.to_string()))?
        };
        let preset = kind.preset();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = {
            let mut st = lock(&self.exports);
            if st.active.is_some() {
                return Err(CommandError::new("busy", "an export is already running"));
            }
            st.next_id = st.next_id.saturating_add(1);
            let id = st.next_id;
            st.active = Some(Active {
                id,
                key: key.clone(),
                out: out.clone(),
                preset: preset.id(),
                cancel: cancel.clone(),
                progress: Progress::default(),
                started: Instant::now(),
            });
            id
        };
        let job = ExportJob {
            id,
            clip,
            out: out.clone(),
            render,
            preset: preset.id().to_string(),
            encoders: encoders.names(),
            discord_limit_bytes: u64::from(settings.export.discord_limit_mb)
                .saturating_mul(1_000_000),
            pretend_overshoot: 0.0,
        };
        let me = self.me.clone();
        let exe = self.media_worker.clone();
        let root = lib.root().to_path_buf();
        let spawned = std::thread::Builder::new()
            .name(format!("klip-export-{id}"))
            .spawn(move || export_thread(&me, exe, job, kind, key, root, cancel));
        if let Err(e) = spawned {
            lock(&self.exports).active = None;
            return Err(CommandError::new(
                "failed",
                format!("no thread for the export: {e}"),
            ));
        }
        Ok((id, out))
    }

    /// Progress from the export thread: remember it, tell the UI now and then.
    fn export_progress(&self, id: u64, p: Progress, last_emit: &mut Instant, force: bool) {
        let (key, preset, pass_changed) = {
            let mut st = lock(&self.exports);
            let Some(a) = st.active.as_mut().filter(|a| a.id == id) else {
                return;
            };
            let changed = a.progress.pass != p.pass;
            a.progress = p;
            (a.key.clone(), a.preset, changed)
        };
        if force || pass_changed || last_emit.elapsed() >= PROGRESS_EVERY {
            *last_emit = Instant::now();
            self.emit(&Event::ExportProgress {
                id,
                key,
                preset: preset.to_string(),
                pass: p.pass,
                frames_done: p.frames_done,
                frames_total: p.frames_total,
            });
        }
    }
}

fn export_thread(
    me: &Weak<Inner>,
    exe: Option<PathBuf>,
    job: ExportJob,
    kind: ExportKind,
    key: String,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    let id = job.id;
    let preset = kind.preset();
    let mut worker = match exe {
        Some(exe) => MediaWorker::isolated(exe),
        None => MediaWorker::in_process(),
    };
    let mut last_emit = Instant::now();
    let mut progress = |p: Progress| {
        if let Some(inner) = me.upgrade() {
            inner.export_progress(id, p, &mut last_emit, false);
        }
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        worker.export(&job, &mut progress, &cancel, EXPORT_INACTIVITY)
    }))
    .unwrap_or_else(|_| Err("export thread panicked".into()));
    drop(worker);
    let Some(inner) = me.upgrade() else { return };
    let (ok, path, error, summary) = match &result {
        Ok(s) => (
            true,
            Some(s.path.to_string_lossy().into_owned()),
            None,
            serde_json::to_value(s).unwrap_or(Value::Null),
        ),
        Err(e) => (false, None, Some(e.clone()), Value::Null),
    };
    let done = json!({
        "id": id,
        "key": key,
        "preset": preset.id(),
        "ok": ok,
        "path": path,
        "error": error,
        "summary": summary,
    });
    {
        let mut st = lock(&inner.exports);
        if st.active.as_ref().is_some_and(|a| a.id == id) {
            st.active = None;
        }
        st.last = Some(done.clone());
    }
    tracing::info!(id, %key, preset = preset.id(), ok, "export finished");
    inner.emit(&Event::ExportDone {
        id,
        key: key.clone(),
        preset: preset.id().to_string(),
        ok,
        path: path.clone(),
        error,
        summary,
    });
    if ok && kind != ExportKind::Proxy {
        inner.emit(&Event::LibraryChanged {
            reason: "exported".into(),
        });
        let lib = Library::new(root.clone());
        if let Some(new_key) = path.as_deref().and_then(|p| lib.key(Path::new(p)).ok()) {
            inner.queue_new_clip(&root, new_key);
        }
    }
}
