//! Export commands (M8.4, M9.4): `export.start {path, preset}` renders a clip's project
//! (a montage too: its clips must all be there, `missing` otherwise) with a preset, or
//! with `preset: "custom"` and `custom: {width, height, fps, bitrate_kbps, codec}`, into
//! `<name> - <Preset>.mp4` next to the clip (never overwriting), on its own
//! thread through the media worker; `export.cancel {id}`; `export.status`. One export
//! runs at a time (`busy` otherwise). Progress comes as `export_progress` events (at
//! most a few per second), the end as one `export_done`; a finished export is indexed
//! by the library and gets its thumbnails. The editor's preview proxy is the same job
//! with the `proxy` preset, written to `.klip/proxies/`, in a slot of its own: making a
//! preview never blocks an export, nor an export a preview (human check, 2026-10-10).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use klip_control::CommandError;
use klip_edit::compose::LayerKind;
use klip_edit::{Composition, Project, Source, compose};
use klip_export::{CUSTOM_KBPS, CustomOutput, MAX_FPS, MAX_SIDE, OutCodec, Preset, Progress};
use klip_library::{ClipInfo, Library};
use serde_json::{Value, json};

use super::editor::{library_file, missing_error, source_of};
use super::library::str_arg;
use super::{Inner, lock};
use crate::config;
use crate::events::Event;
use crate::media_worker::{EXPORT_INACTIVITY, ExportJob, MediaWorker};

/// Where the composition's other sources are: library clips under the library's root
/// (a key that would leave it is left out, and the export reports it missing), files
/// and pictures where the project says.
pub(super) fn media_of(lib: &Library, comp: &Composition) -> Vec<(Source, PathBuf)> {
    let mut out: Vec<(Source, PathBuf)> = Vec::new();
    for l in &comp.layers {
        if l.source == Source::Main || out.iter().any(|(s, _)| *s == l.source) {
            continue;
        }
        let path = match &l.source {
            Source::Main => None,
            Source::Library { key } => library_file(lib, key),
            Source::File { path } | Source::Image { path } => Some(PathBuf::from(path)),
        };
        if let Some(p) = path {
            out.push((l.source.clone(), p));
        }
    }
    out
}

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
        let preset = if preset_id == "custom" {
            Preset::Custom(custom_arg(args)?)
        } else {
            Preset::ALL
                .into_iter()
                .find(|p| p.id() == preset_id)
                .ok_or_else(|| CommandError::bad_args("unknown preset"))?
        };
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let info = self.clip_info(&lib, &path, &key)?;
        let (id, out) =
            self.start_export(ExportKind::Preset(preset), &lib, path, key, &info, false)?;
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
    /// the proxy; `then_copy` puts the result on the clipboard (M11.5). Returns the job
    /// id and where the file will be.
    pub(super) fn start_export(
        &self,
        kind: ExportKind,
        lib: &Library,
        clip: PathBuf,
        key: String,
        info: &ClipInfo,
        then_copy: bool,
    ) -> Result<(u64, PathBuf), CommandError> {
        if lock(&self.exports).slot(kind).is_some() {
            return Err(busy(kind));
        }
        let source = source_of(info);
        let (composition, out) = match kind {
            ExportKind::Preset(p) => {
                let edit = self.load_project(lib, &key, &source)?;
                let composition = edit.composition(&source);
                // A clip of the montage that is gone fails now, by name (muted sound
                // is never read).
                let played = |k: &str| {
                    composition.layers.iter().any(|l| {
                        matches!(&l.source, Source::Library { key } if key == k)
                            && !(l.muted && matches!(l.kind, LayerKind::Audio | LayerKind::Music))
                    })
                };
                let gone: Vec<&str> = edit
                    .missing()
                    .map(|c| c.key.as_str())
                    .filter(|k| played(k))
                    .collect();
                if !gone.is_empty() {
                    return Err(missing_error(gone.into_iter()));
                }
                let out = lib
                    .unique_sibling(&clip, p.name())
                    .map_err(|e| CommandError::new("failed", e.to_string()))?;
                (composition, out)
            }
            ExportKind::Proxy => {
                let out = lib
                    .proxy_path(&key)
                    .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
                (compose(&Project::new(source.duration_ms), &source), out)
            }
        };
        if composition.duration_ms == 0 {
            return Err(CommandError::bad_args("the edit is empty"));
        }
        let media = media_of(lib, &composition);
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
            composition,
            media,
            preset: preset.id().to_string(),
            custom: match preset {
                Preset::Custom(c) => Some(c),
                _ => None,
            },
            encoders: encoders.names(),
            discord_limit_bytes: u64::from(settings.export.discord_limit_mb)
                .saturating_mul(1_000_000),
            pretend_overshoot: 0.0,
        };
        let me = self.me.clone();
        let exe = self.media_worker.clone();
        let ctx = JobCtx {
            kind,
            key,
            root: lib.root().to_path_buf(),
            then_copy,
        };
        let spawned = std::thread::Builder::new()
            .name(format!("klip-export-{id}"))
            .spawn(move || export_thread(&me, exe, job, ctx, cancel));
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

/// `args.custom` of a Custom export, every number checked.
fn custom_arg(args: &Value) -> Result<CustomOutput, CommandError> {
    let c = args
        .get("custom")
        .filter(|v| v.is_object())
        .ok_or_else(|| CommandError::bad_args("custom must be an object"))?;
    let num = |name: &str, lo: u32, hi: u32| {
        c.get(name)
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| (lo..=hi).contains(v))
            .ok_or_else(|| CommandError::bad_args(format!("custom.{name} must be {lo}…{hi}")))
    };
    let codec: OutCodec = c
        .get("codec")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| CommandError::bad_args("custom.codec must be h264, hevc or av1"))?;
    Ok(CustomOutput {
        width: num("width", 2, MAX_SIDE)?,
        height: num("height", 2, MAX_SIDE)?,
        fps: num("fps", 1, MAX_FPS)?,
        bitrate_kbps: num("bitrate_kbps", CUSTOM_KBPS.0, CUSTOM_KBPS.1)?,
        codec,
    })
}

/// What an export thread does around the encoding.
struct JobCtx {
    kind: ExportKind,
    key: String,
    /// The library root (the new file is indexed there).
    root: PathBuf,
    /// Put the result on the clipboard (`share.discord`).
    then_copy: bool,
}

fn export_thread(
    me: &Weak<Inner>,
    exe: Option<PathBuf>,
    job: ExportJob,
    ctx: JobCtx,
    cancel: Arc<AtomicBool>,
) {
    let JobCtx {
        kind,
        key,
        root,
        then_copy,
    } = ctx;
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
    let (copied, copy_error) = match path.as_deref() {
        Some(p) if then_copy => match klip_platform::copy_files(&[PathBuf::from(p)]) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        },
        _ => (false, None),
    };
    let done = json!({
        "id": id,
        "key": key,
        "preset": preset.id(),
        "ok": ok,
        "path": path,
        "error": error,
        "summary": summary,
        "copied": copied,
        "copy_error": copy_error,
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
        copied,
        copy_error,
    });
    if ok && kind != ExportKind::Proxy {
        inner.emit(&Event::LibraryChanged {
            reason: "exported".into(),
        });
        let lib = Library::new(root.clone());
        if let Some(new_key) = path
            .as_deref()
            .and_then(|p| lib.key(std::path::Path::new(p)).ok())
        {
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
    fn custom_settings_are_checked() {
        let ok = json!({"custom": {"width": 1280, "height": 720, "fps": 30, "bitrate_kbps": 6000, "codec": "hevc"}});
        assert_eq!(
            custom_arg(&ok).unwrap(),
            CustomOutput {
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_kbps: 6000,
                codec: OutCodec::Hevc
            }
        );
        for (field, bad) in [
            ("width", json!(1)),
            ("width", json!(5000)),
            ("height", json!(-2)),
            ("fps", json!(0)),
            ("fps", json!(241)),
            ("bitrate_kbps", json!(50)),
            ("bitrate_kbps", json!("6000")),
            ("codec", json!("x264")),
            ("codec", json!(null)),
        ] {
            let mut a = ok.clone();
            a["custom"][field] = bad;
            let e = custom_arg(&a).unwrap_err();
            assert_eq!(e.code, "bad_args", "{field}");
            assert!(e.message.contains(field), "{}", e.message);
        }
        assert!(custom_arg(&json!({})).is_err());
        assert!(custom_arg(&json!({"custom": [1]})).is_err());
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
