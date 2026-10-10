//! Export commands (M8.4): `export.start {path, preset}` renders a clip's project with a
//! preset into `<name> - <Preset>.mp4` next to the clip (never overwriting), on its own
//! thread through the media worker; `export.cancel {id}`; `export.status`. One export
//! runs at a time (`busy` otherwise). Progress comes as `export_progress` events (at
//! most a few per second), the end as one `export_done`; a finished export is indexed
//! by the library and gets its thumbnails. The editor's preview proxy is the same job
//! with the `proxy` preset, written to `.klip/proxies/`, in a slot of its own: making a
//! preview never blocks an export, nor an export a preview (human check, 2026-10-10).

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

/// A job that runs now.
struct Active {
    id: u64,
    key: String,
    out: PathBuf,
    preset: &'static str,
    cancel: Arc<AtomicBool>,
    progress: Progress,
    started: Instant,
}

impl Active {
    fn status(&self) -> Value {
        json!({
            "id": self.id,
            "key": self.key,
            "out": self.out.to_string_lossy(),
            "preset": self.preset,
            "pass": self.progress.pass,
            "frames_done": self.progress.frames_done,
            "frames_total": self.progress.frames_total,
            "elapsed_ms": self.started.elapsed().as_millis() as u64,
        })
    }
}

/// Export state behind `Inner::exports`: one user export and one preview proxy at a
/// time, side by side.
#[derive(Default)]
pub(super) struct ExportState {
    /// The user export that runs now.
    active: Option<Active>,
    /// The preview proxy being made now.
    proxy: Option<Active>,
    /// The last finished export (what `export_done` said).
    last: Option<Value>,
    next_id: u64,
}

impl ExportState {
    fn slot(&mut self, kind: ExportKind) -> &mut Option<Active> {
        match kind {
            ExportKind::Preset(_) => &mut self.active,
            ExportKind::Proxy => &mut self.proxy,
        }
    }

    fn running(&self) -> impl Iterator<Item = &Active> {
        self.active.iter().chain(self.proxy.iter())
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut Active> {
        [&mut self.active, &mut self.proxy]
            .into_iter()
            .find_map(|s| s.as_mut().filter(|a| a.id == id))
    }

    /// Take `kind`'s slot for a new job (`busy` while that kind runs); the job's id.
    fn claim(
        &mut self,
        kind: ExportKind,
        make: impl FnOnce(u64) -> Active,
    ) -> Result<u64, CommandError> {
        if self.slot(kind).is_some() {
            return Err(busy(kind));
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        *self.slot(kind) = Some(make(id));
        Ok(id)
    }

    /// Free the slot of job `id`, whichever kind it is.
    fn release(&mut self, id: u64) {
        for slot in [&mut self.active, &mut self.proxy] {
            if slot.as_ref().is_some_and(|a| a.id == id) {
                *slot = None;
            }
        }
    }
}

fn busy(kind: ExportKind) -> CommandError {
    match kind {
        ExportKind::Preset(_) => CommandError::new("busy", "an export is already running"),
        ExportKind::Proxy => CommandError::new("busy", "a preview is already being made"),
    }
}

impl Inner {
    /// The id of a running export of `key` with `preset`, if any.
    pub(super) fn export_running_for(&self, key: &str, preset: &str) -> Option<u64> {
        let st = lock(&self.exports);
        st.running()
            .find(|a| a.key == key && a.preset == preset)
            .map(|a| a.id)
    }

    /// Stop the running jobs (engine shutdown).
    pub(super) fn cancel_export(&self) {
        for a in lock(&self.exports).running() {
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
        match st.running().find(|a| a.id == id) {
            Some(a) => {
                a.cancel.store(true, Ordering::Relaxed);
                Ok(json!({ "id": id }))
            }
            None => Err(CommandError::new("not_found", "no such export")),
        }
    }

    pub(super) fn export_status(&self, _args: &Value) -> Result<Value, CommandError> {
        let st = lock(&self.exports);
        Ok(json!({
            "active": st.active.as_ref().map(Active::status),
            "proxy": st.proxy.as_ref().map(Active::status),
            "last": st.last,
        }))
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
        if lock(&self.exports).slot(kind).is_some() {
            return Err(busy(kind));
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
        let id = lock(&self.exports).claim(kind, |id| Active {
            id,
            key: key.clone(),
            out: out.clone(),
            preset: preset.id(),
            cancel: cancel.clone(),
            progress: Progress::default(),
            started: Instant::now(),
        })?;
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
            lock(&self.exports).release(id);
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
            let Some(a) = st.find_mut(id) else {
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
        st.release(id);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64) -> Active {
        Active {
            id,
            key: "Game/Clip.mp4".into(),
            out: PathBuf::from("Clip - Discord.mp4"),
            preset: "discord",
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Progress::default(),
            started: Instant::now(),
        }
    }

    #[test]
    fn a_proxy_and_an_export_run_side_by_side() {
        let mut st = ExportState::default();
        let export = ExportKind::Preset(Preset::Discord);
        // The preview proxy of an HEVC clip is being made: an export still starts.
        let proxy = st.claim(ExportKind::Proxy, job).unwrap();
        let first = st.claim(export, job).unwrap();
        assert_ne!(proxy, first);
        assert_eq!(st.running().count(), 2);
        // One of each kind at a time.
        let e = st.claim(export, job).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            ("busy", "an export is already running")
        );
        assert_eq!(st.claim(ExportKind::Proxy, job).unwrap_err().code, "busy");
        // Freeing one leaves the other running.
        st.release(proxy);
        assert!(st.proxy.is_none());
        assert_eq!(st.active.as_ref().map(|a| a.id), Some(first));
        assert!(st.claim(ExportKind::Proxy, job).is_ok());
        st.release(first);
        assert!(st.active.is_none());
        assert!(st.claim(export, job).is_ok());
    }
}
