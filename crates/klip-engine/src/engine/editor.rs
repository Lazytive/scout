//! Editor commands (M8.4, M9.4): a clip's project file and what the preview plays.
//!
//! `editor.project.get {path}` → the project (a new one when none is saved), its repair
//! issues, the clip's `SourceInfo`, and the other library clips it places (`clips`: a
//! montage lives under its first clip; a clip that is gone is reported `missing`, never
//! dropped). `editor.project.set {path, project}` → the project repaired and saved to
//! `.klip/projects/<key>.json`; the project it replaces is kept as a history snapshot
//! (`.klip/history/<key>/<unix ms>.json`, at most one a minute, [`HISTORY_MAX`] kept):
//! `editor.history.list|restore`. `editor.montage {path, add}` appends clips.
//! `editor.frame {path, out_ms, width}` draws one frame through the export's renderer
//! in the media worker. `editor.proxy {path}` → what the preview should play: the clip
//! itself for H.264, otherwise its H.264 proxy (`ready` with its path, or `making`
//! with the job id of the export that makes it). `editor.audio {path}` → the clip's
//! audio tracks one per file for the preview's mixer (`.klip/audio/<key>/<n>.m4a`, made
//! in the media worker, remade when the clip changes). `editor.files {path}` → the
//! pictures and sounds the project places, checked before the app lets the preview
//! read them.

use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use klip_control::CommandError;
use klip_edit::{
    Composition, MAX_FILE_BYTES, Project, Source, SourceEvent, SourceInfo, TextLayer, compose_with,
};
use klip_library::{ClipInfo, Library, audio_track_names, probe_file};
use serde_json::{Value, json};

use super::export::{ExportKind, media_of};
use super::library::str_arg;
use super::{Inner, lock};
use crate::media_worker::{AudioJob, FrameJob, audio_timeout, write_atomic};

/// Snapshots kept per project.
pub const HISTORY_MAX: usize = 30;
/// A save keeps the project it replaces when the newest snapshot is older than this.
const HISTORY_EVERY_MS: u64 = 60_000;
/// Snapshot files looked at, at most (a hostile folder may hold many).
const HISTORY_SCAN_MAX: usize = 1024;
/// How long a frame may take (it opens and seeks every visible clip).
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest frame JPEG read back from the worker.
const MAX_FRAME_BYTES: u64 = 32 << 20;
/// Project files read when a renamed clip is looked for in montages.
const RETARGET_SCAN_MAX: usize = 100_000;
/// What `.klip/audio/<key>/` holds besides the tracks: which file they came from.
const AUDIO_MANIFEST: &str = "tracks.json";
/// Picture extensions a sticker may have (what the export's decoder reads).
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
/// Sound extensions a sound or music clip may have.
const SOUND_EXTS: &[&str] = &[
    "mp3", "wav", "m4a", "aac", "ogg", "oga", "opus", "flac", "mp4", "webm",
];
/// Most file sources `editor.files` checks.
const MAX_FILES: usize = 512;

/// The editor's view of a clip: the index entry when it has one, a probe otherwise.
pub(super) fn source_of(info: &ClipInfo) -> SourceInfo {
    SourceInfo {
        width: info.width,
        height: info.height,
        duration_ms: info.duration_ms.unwrap_or(0),
        fps: info.fps,
        bookmarks_ms: info.bookmarks_ms.clone(),
        events: info
            .events
            .iter()
            .map(|e| SourceEvent {
                at_ms: e.at_ms,
                title: e.title.clone(),
            })
            .collect(),
    }
}

fn issues_json(issues: &[klip_edit::Issue]) -> Value {
    serde_json::to_value(issues).unwrap_or(Value::Array(Vec::new()))
}

/// A project fitted to its clip: the clip's length wins, then repaired.
pub(super) fn fit_project(
    mut project: Project,
    source: &SourceInfo,
) -> (Project, Vec<klip_edit::Issue>) {
    if source.duration_ms > 0 {
        project.source_ms = source.duration_ms;
    }
    let issues = project.repair();
    (project, issues)
}

/// The file of library clip `key`: under the library's root (a key that would leave
/// it has none).
pub(super) fn library_file(lib: &Library, key: &str) -> Option<PathBuf> {
    let rel = Path::new(key);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| lib.root().join(rel))
}

/// Another library clip a project places (a montage), as found now.
pub(super) struct Placed {
    pub key: String,
    /// `ok`, `missing` (no such file) or `unreadable`.
    pub status: &'static str,
    pub info: Option<SourceInfo>,
    /// The clip's file, when it is there.
    pub path: Option<PathBuf>,
}

/// A project with what the editor and the renderer need around it.
pub(super) struct Edit {
    pub project: Project,
    pub issues: Vec<klip_edit::Issue>,
    pub status: &'static str,
    pub clips: Vec<Placed>,
}

impl Edit {
    /// The composition the renderer draws (montage clips' chapters included).
    pub(super) fn composition(&self, source: &SourceInfo) -> Composition {
        let known: Vec<(String, SourceInfo)> = self
            .clips
            .iter()
            .filter_map(|c| c.info.clone().map(|i| (c.key.clone(), i)))
            .collect();
        compose_with(&self.project, source, &known)
    }

    /// The placed library clips that can't be read.
    pub(super) fn missing(&self) -> impl Iterator<Item = &Placed> {
        self.clips.iter().filter(|c| c.info.is_none())
    }
}

/// Error for clips of the edit that are gone: names only (folders may hold personal
/// names).
pub(super) fn missing_error<'a>(keys: impl Iterator<Item = &'a str>) -> CommandError {
    let names: Vec<String> = keys
        .map(|k| k.rsplit('/').next().unwrap_or(k).to_string())
        .collect();
    CommandError::new(
        "missing",
        format!("missing from the clip folder: {}", names.join(", ")),
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The snapshots in `dir`, newest first: `(id = unix ms, path, bytes)`.
fn snapshots(dir: &Path) -> Vec<(u64, PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten().take(HISTORY_SCAN_MAX) {
        let name = e.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        let bytes = e.metadata().map_or(0, |m| m.len());
        out.push((id, e.path(), bytes));
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.0));
    out
}

/// Keep a copy of the saved project `file` (if there is one) in `dir`: always when
/// `force`, otherwise when the newest snapshot is older than [`HISTORY_EVERY_MS`]; then
/// keep the newest [`HISTORY_MAX`]. The snapshot's id, if one was made. Call under
/// `library_lock`.
fn snapshot(file: &Path, dir: &Path, force: bool) -> std::io::Result<Option<u64>> {
    if !file.is_file() {
        return Ok(None);
    }
    let list = snapshots(dir);
    let now = now_ms();
    let newest = list.first().map(|(id, _, _)| *id);
    if !force && newest.is_some_and(|id| now.saturating_sub(id) < HISTORY_EVERY_MS) {
        return Ok(None);
    }
    fs::create_dir_all(dir)?;
    // Ids only grow, even when the clock goes back.
    let id = now.max(newest.map_or(0, |n| n.saturating_add(1)));
    let tmp = dir.join(format!("{id}.json.tmp"));
    let res = fs::copy(file, &tmp).and_then(|_| fs::rename(&tmp, dir.join(format!("{id}.json"))));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res?;
    for (_, old, _) in snapshots(dir).into_iter().skip(HISTORY_MAX) {
        let _ = fs::remove_file(old);
    }
    Ok(Some(id))
}

/// Ids of frame jobs (each worker answers by id).
static FRAME_IDS: AtomicU64 = AtomicU64::new(0);

/// The clip a set of split audio tracks came from: size and modification time.
fn file_stamp(path: &Path) -> Option<(u64, u128)> {
    let m = fs::metadata(path).ok()?;
    let modified = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some((m.len(), modified))
}

/// The extension of `path`, lowercased.
fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Why a project's file source can't be offered to the preview, if it can't.
fn file_problem(path: &Path, kind: &str) -> Option<&'static str> {
    if !path.is_absolute() {
        return Some("not a full path");
    }
    let ext = extension(path);
    let allowed = if kind == "image" {
        IMAGE_EXTS
    } else {
        SOUND_EXTS
    };
    if !allowed.contains(&ext.as_str()) {
        return Some("not a picture or sound file");
    }
    if !path.is_file() {
        return Some("missing");
    }
    None
}

impl Inner {
    /// What is known about a clip (the index, else the file).
    pub(super) fn clip_info(
        &self,
        lib: &Library,
        path: &Path,
        key: &str,
    ) -> Result<ClipInfo, CommandError> {
        {
            let state = lock(&self.library);
            if state.root() == Some(lib.root())
                && let Some(info) = state.info_of(key)
            {
                return Ok(info);
            }
        }
        probe_file(path).map_err(|e| CommandError::new("failed", e))
    }

    /// The other library clips `project` places, each looked up once.
    fn placed_clips(&self, lib: &Library, project: &Project) -> Vec<Placed> {
        let mut keys: Vec<&str> = Vec::new();
        for c in project.advanced.tracks.iter().flat_map(|t| &t.clips) {
            if let Source::Library { key } = &c.source
                && !keys.contains(&key.as_str())
            {
                keys.push(key);
            }
        }
        keys.into_iter()
            .map(|key| {
                let file = library_file(lib, key).filter(|p| p.is_file());
                let (status, info) = match &file {
                    None => ("missing", None),
                    Some(p) => match self.clip_info(lib, p, key) {
                        Ok(i) => ("ok", Some(source_of(&i))),
                        Err(_) => ("unreadable", None),
                    },
                };
                Placed {
                    key: key.to_string(),
                    status,
                    info,
                    path: file,
                }
            })
            .collect()
    }

    /// `project` fitted to its clip and to the montage clips it places.
    fn fit_edit(
        &self,
        lib: &Library,
        mut project: Project,
        source: &SourceInfo,
        status: &'static str,
    ) -> Edit {
        let clips = self.placed_clips(lib, &project);
        let mut issues = project.advanced.fit_library_clips(|key| {
            clips
                .iter()
                .find(|c| c.key == key)
                .and_then(|c| c.info.as_ref())
                .map(|i| i.duration_ms)
        });
        let (project, more) = fit_project(project, source);
        issues.extend(more);
        Edit {
            project,
            issues,
            status,
            clips,
        }
    }

    /// The saved project of `key`, or a new one, fitted.
    pub(super) fn load_project(
        &self,
        lib: &Library,
        key: &str,
        source: &SourceInfo,
    ) -> Result<Edit, CommandError> {
        let path = lib
            .project_path(key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        let (project, issues, status) = if path.is_file() {
            let loaded = {
                let _guard = lock(&self.library_lock);
                klip_edit::load(&path)
            };
            let status = match loaded.status {
                klip_edit::LoadStatus::Loaded => "saved",
                klip_edit::LoadStatus::Repaired => "repaired",
                klip_edit::LoadStatus::Reset => "reset",
                klip_edit::LoadStatus::Missing => "new",
            };
            (loaded.project, loaded.issues, status)
        } else {
            (Project::new(source.duration_ms), Vec::new(), "new")
        };
        let mut edit = self.fit_edit(lib, project, source, status);
        let mut all = issues;
        all.append(&mut edit.issues);
        edit.issues = all;
        Ok(edit)
    }

    /// A project sent by the UI (`args.project`), checked and fitted.
    fn project_arg(
        &self,
        lib: &Library,
        args: &Value,
        source: &SourceInfo,
    ) -> Result<Edit, CommandError> {
        let value = args
            .get("project")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| CommandError::bad_args("project must be an object"))?;
        let bytes = serde_json::to_vec(&value)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if bytes as u64 > MAX_FILE_BYTES {
            return Err(CommandError::bad_args("project is too big"));
        }
        let (project, issues) = Project::from_value(value);
        let mut edit = self.fit_edit(lib, project, source, "saved");
        let mut all = issues;
        all.append(&mut edit.issues);
        edit.issues = all;
        Ok(edit)
    }

    /// Save `project` as `key`'s project, keeping the one it replaces as a snapshot
    /// (`force`: always). Nothing is written when the file already says the same.
    fn save_project(
        &self,
        lib: &Library,
        key: &str,
        project: &Project,
        force_snapshot: bool,
    ) -> Result<(), CommandError> {
        let failed = |e: std::io::Error| CommandError::new("failed", e.to_string());
        let file = lib
            .project_path(key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        let history = lib
            .history_dir(key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        let mut text = serde_json::to_vec_pretty(project)
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        text.push(b'\n');
        let _guard = lock(&self.library_lock);
        if !force_snapshot && fs::read(&file).is_ok_and(|old| old == text) {
            return Ok(());
        }
        if let Err(e) = snapshot(&file, &history, force_snapshot) {
            tracing::warn!(error = %e, "could not keep a project snapshot");
        }
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).map_err(failed)?;
        }
        klip_edit::save(&file, project).map_err(failed)
    }

    fn project_json(
        key: &str,
        path: &Path,
        edit: &Edit,
        source: &SourceInfo,
    ) -> Result<Value, CommandError> {
        let value = edit
            .project
            .to_value()
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        let clips: Vec<Value> = edit
            .clips
            .iter()
            .map(|c| {
                json!({
                    "key": c.key,
                    "name": c.key.rsplit('/').next().unwrap_or(&c.key),
                    "status": c.status,
                    "source": c.info,
                    "path": c.path.as_ref().map(|p| p.to_string_lossy()),
                })
            })
            .collect();
        Ok(json!({
            "key": key,
            "path": path.to_string_lossy(),
            "project": value,
            "issues": issues_json(&edit.issues),
            "status": edit.status,
            "source": source,
            "clips": clips,
        }))
    }

    pub(super) fn editor_project_get(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let edit = self.load_project(&lib, &key, &source)?;
        Self::project_json(&key, &path, &edit, &source)
    }

    pub(super) fn editor_project_set(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let edit = self.project_arg(&lib, args, &source)?;
        self.save_project(&lib, &key, &edit.project, false)?;
        Self::project_json(&key, &path, &edit, &source)
    }

    /// `editor.montage {path, add: [paths]}`: the clips `add` appended, whole, after
    /// the end of `path`'s edit on its first video track; saved.
    pub(super) fn editor_montage(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let add = args
            .get("add")
            .and_then(Value::as_array)
            .filter(|a| (1..=klip_edit::advanced::MAX_CLIPS).contains(&a.len()))
            .ok_or_else(|| CommandError::bad_args("add must be a list of 1…256 clips"))?;
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let mut edit = self.load_project(&lib, &key, &source)?;
        let mut clips: Vec<(Source, u64)> = Vec::with_capacity(add.len());
        for a in add {
            let p = a
                .as_str()
                .ok_or_else(|| CommandError::bad_args("add must hold clip paths"))?;
            let (p, k) = self.clip_arg(&lib, p)?;
            let len = self.clip_info(&lib, &p, &k)?.duration_ms.unwrap_or(0);
            let src = if k == key {
                Source::Main
            } else {
                Source::Library { key: k }
            };
            clips.push((src, len));
        }
        let end = edit.composition(&source).duration_ms;
        edit.project
            .advanced
            .append_clips(&clips, end)
            .map_err(CommandError::bad_args)?;
        let edit = self.fit_edit(&lib, edit.project, &source, "saved");
        self.save_project(&lib, &key, &edit.project, false)?;
        Self::project_json(&key, &path, &edit, &source)
    }

    /// `editor.frame {path, out_ms, width, project?}`: the frame the export encodes at
    /// `out_ms` (the saved project, or `project` as the UI has it now), as a JPEG.
    pub(super) fn editor_frame(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let out_ms = args
            .get("out_ms")
            .and_then(Value::as_u64)
            .ok_or_else(|| CommandError::bad_args("out_ms must be a number"))?;
        let width = args
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|w| u32::try_from(w).ok())
            .filter(|w| (klip_export::MIN_FRAME_WIDTH..=klip_export::MAX_SIDE).contains(w))
            .ok_or_else(|| CommandError::bad_args("width must be 16…4096"))?;
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let edit = match args.get("project") {
            Some(v) if !v.is_null() => self.project_arg(&lib, args, &source)?,
            _ => self.load_project(&lib, &key, &source)?,
        };
        let composition = edit.composition(&source);
        if composition.duration_ms == 0 {
            return Err(CommandError::bad_args("the edit is empty"));
        }
        let t = out_ms.min(composition.duration_ms.saturating_sub(1));
        let shown: Vec<&str> = composition
            .visible_at(t)
            .into_iter()
            .filter_map(|l| match &l.source {
                Source::Library { key } => Some(key.as_str()),
                _ => None,
            })
            .collect();
        let gone: Vec<&str> = edit
            .missing()
            .map(|c| c.key.as_str())
            .filter(|k| shown.contains(k))
            .collect();
        if !gone.is_empty() {
            return Err(missing_error(gone.into_iter()));
        }
        let id = FRAME_IDS.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let out = std::env::temp_dir().join(format!("klip-frame-{}-{id}.jpg", std::process::id()));
        let job = FrameJob {
            id,
            clip: path,
            out: out.clone(),
            media: media_of(&lib, &composition),
            composition,
            frame_ms: t,
            width,
        };
        let reply = lock(&self.frames).frame(&job, FRAME_TIMEOUT);
        let read = reply.and_then(|r| {
            if !r.ok {
                return Err(r.error.unwrap_or_else(|| "failed".into()));
            }
            let mut jpeg = Vec::new();
            fs::File::open(&out)
                .and_then(|f| f.take(MAX_FRAME_BYTES).read_to_end(&mut jpeg))
                .map_err(|e| e.to_string())?;
            Ok((jpeg, r))
        });
        let _ = fs::remove_file(&out);
        let (jpeg, r) = read.map_err(|e| CommandError::new("failed", e))?;
        Ok(json!({
            "key": key,
            "out_ms": r.out_ms,
            "width": r.width,
            "height": r.height,
            "jpeg": base64::engine::general_purpose::STANDARD.encode(&jpeg),
        }))
    }

    /// `editor.history.list {path}`: the project's snapshots, newest first.
    pub(super) fn editor_history_list(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (_, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let dir = lib
            .history_dir(&key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        let list = {
            let _guard = lock(&self.library_lock);
            snapshots(&dir)
        };
        let snapshots: Vec<Value> = list
            .iter()
            .map(|(id, _, bytes)| json!({ "id": id, "saved_unix_ms": id, "bytes": bytes }))
            .collect();
        Ok(json!({ "key": key, "snapshots": snapshots }))
    }

    /// `editor.history.restore {path, id}`: snapshot `id` becomes the project again; the
    /// project it replaces is kept as a snapshot.
    pub(super) fn editor_history_restore(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| CommandError::bad_args("id must be a number"))?;
        let file = lib
            .history_dir(&key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?
            .join(format!("{id}.json"));
        let loaded = {
            let _guard = lock(&self.library_lock);
            if !file.is_file() {
                return Err(CommandError::new("not_found", "no such snapshot"));
            }
            klip_edit::load(&file)
        };
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let mut edit = self.fit_edit(&lib, loaded.project, &source, "restored");
        let mut all = loaded.issues;
        all.append(&mut edit.issues);
        edit.issues = all;
        self.save_project(&lib, &key, &edit.project, true)?;
        Self::project_json(&key, &path, &edit, &source)
    }

    /// A clip was renamed: montages that place it follow it to its new key.
    pub(super) fn retarget_montages(&self, lib: &Library, from: &str, to: &str) {
        let Some(dir) = lib
            .project_path(to)
            .and_then(|p| p.parent().map(Path::to_path_buf))
        else {
            return;
        };
        let Ok(needle) = serde_json::to_string(from) else {
            return;
        };
        let _guard = lock(&self.library_lock);
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        for e in entries.flatten().take(RETARGET_SCAN_MAX) {
            let file = e.path();
            if file.extension().and_then(|x| x.to_str()) != Some("json")
                || e.metadata().map_or(true, |m| m.len() > MAX_FILE_BYTES)
            {
                continue;
            }
            let Ok(bytes) = fs::read(&file) else {
                continue;
            };
            if !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                continue;
            }
            let loaded = klip_edit::load(&file);
            if !matches!(
                loaded.status,
                klip_edit::LoadStatus::Loaded | klip_edit::LoadStatus::Repaired
            ) {
                continue;
            }
            let mut project = loaded.project;
            let mut changed = false;
            for c in project
                .advanced
                .tracks
                .iter_mut()
                .flat_map(|t| t.clips.iter_mut())
            {
                if let Source::Library { key } = &mut c.source
                    && key == from
                {
                    *key = to.to_string();
                    changed = true;
                }
            }
            if changed && let Err(e) = klip_edit::save(&file, &project) {
                tracing::warn!(error = %e, "could not update a montage after a rename");
            }
        }
    }

    /// A text layer as the export draws it, for the preview: `klip_edit::render_layer`
    /// at the output size, as a PNG (base64) with its place in the frame.
    pub(super) fn editor_text_render(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        self.clip_arg(&lib, str_arg(args, "path")?)?;
        let layer: TextLayer = args
            .get("layer")
            .cloned()
            .filter(Value::is_object)
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| CommandError::bad_args("layer must be a text layer"))?;
        let side = |name: &str| -> Result<u32, CommandError> {
            args.get(name)
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| (1..=klip_edit::text::render::MAX_FRAME_SIDE).contains(v))
                .ok_or_else(|| CommandError::bad_args(format!("{name} must be 1..=4096")))
        };
        let (width, height) = (side("width")?, side("height")?);
        let bitmap = klip_edit::render_layer(&layer, width, height)
            .map_err(|e| CommandError::bad_args(e.to_string()))?;
        let png = bitmap
            .png()
            .map_err(|e| CommandError::new("failed", e.to_string()))?;
        Ok(json!({
            "x": bitmap.x,
            "y": bitmap.y,
            "width": bitmap.width,
            "height": bitmap.height,
            "frame_width": bitmap.frame_width,
            "frame_height": bitmap.frame_height,
            "png": base64::engine::general_purpose::STANDARD.encode(png),
        }))
    }

    /// `editor.audio {path}`: the clip's audio tracks as separate files (made once,
    /// remade when the clip's size or time changes).
    pub(super) fn editor_audio(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let dir = lib
            .audio_dir(&key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        let stamp =
            file_stamp(&path).ok_or_else(|| CommandError::new("not_found", "the clip is gone"))?;
        let names = audio_track_names(&path).map_err(|e| CommandError::new("failed", e))?;
        let n = u32::try_from(names.len())
            .unwrap_or(u32::MAX)
            .min(klip_export::audio::MAX_AUDIO_TRACKS);
        let files: Vec<PathBuf> = (0..n)
            .map(|i| klip_export::audio::track_file(&dir, i))
            .collect();
        let manifest = dir.join(AUDIO_MANIFEST);
        let fresh = fs::read(&manifest)
            .ok()
            .filter(|b| b.len() < 4096)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|m| {
                m.get("bytes").and_then(Value::as_u64) == Some(stamp.0)
                    && m.get("modified_ms").and_then(Value::as_str)
                        == Some(stamp.1.to_string().as_str())
                    && m.get("tracks").and_then(Value::as_u64) == Some(u64::from(n))
            })
            && files.iter().all(|f| f.is_file());
        if n > 0 && !fresh {
            let _ = fs::remove_file(&manifest);
            let id = FRAME_IDS.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            let job = AudioJob {
                id,
                clip: path.clone(),
                out_dir: dir.clone(),
                tracks: n,
            };
            let reply = lock(&self.frames)
                .audio(&job, audio_timeout(stamp.0))
                .map_err(|e| CommandError::new("failed", e))?;
            if !reply.ok {
                return Err(CommandError::new(
                    "failed",
                    reply
                        .error
                        .unwrap_or_else(|| "the sound could not be read".into()),
                ));
            }
            let m = json!({ "bytes": stamp.0, "modified_ms": stamp.1.to_string(), "tracks": n });
            write_atomic(&manifest, m.to_string().as_bytes())
                .map_err(|e| CommandError::new("failed", e.to_string()))?;
        }
        let tracks: Vec<Value> = files
            .iter()
            .zip(&names)
            .enumerate()
            .map(|(i, (f, name))| json!({ "index": i, "name": name, "path": f.to_string_lossy() }))
            .collect();
        Ok(json!({ "key": key, "status": "ready", "tracks": tracks }))
    }

    /// `editor.files {path}`: the pictures and sounds the clip's saved project places,
    /// each checked (a full path, a picture or sound extension, there).
    pub(super) fn editor_files(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let edit = self.load_project(&lib, &key, &source)?;
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for c in edit.project.advanced.tracks.iter().flat_map(|t| &t.clips) {
            let (p, kind) = match &c.source {
                Source::File { path } => (path.as_str(), "audio"),
                Source::Image { path } => (path.as_str(), "image"),
                Source::Main | Source::Library { .. } => continue,
            };
            if seen.len() < MAX_FILES && !seen.contains(&(p, kind)) {
                seen.push((p, kind));
            }
        }
        let files: Vec<Value> = seen
            .into_iter()
            .map(|(p, kind)| match file_problem(Path::new(p), kind) {
                None => json!({ "path": p, "kind": kind, "ok": true }),
                Some(why) => json!({ "path": p, "kind": kind, "ok": false, "error": why }),
            })
            .collect();
        Ok(json!({ "key": key, "files": files }))
    }

    /// What the preview plays for `path`.
    pub(super) fn editor_proxy(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let info = self.clip_info(&lib, &path, &key)?;
        if info.codec.as_deref() == Some("H.264") {
            return Ok(json!({ "status": "original", "path": path.to_string_lossy(), "key": key }));
        }
        let proxy = lib
            .proxy_path(&key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        if proxy.is_file() {
            return Ok(json!({ "status": "ready", "path": proxy.to_string_lossy(), "key": key }));
        }
        if let Some(id) = self.export_running_for(&key, "proxy") {
            return Ok(json!({ "status": "making", "id": id, "key": key }));
        }
        let (id, _) = self.start_export(ExportKind::Proxy, &lib, path, key.clone(), &info)?;
        Ok(json!({ "status": "making", "id": id, "key": key }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_coalesce_cap_and_ignore_strangers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.json");
        let hist = dir.path().join("h");
        assert_eq!(
            snapshot(&file, &hist, true).unwrap(),
            None,
            "nothing saved yet"
        );
        fs::write(&file, b"{\"v\":1}").unwrap();
        let first = snapshot(&file, &hist, false).unwrap().unwrap();
        assert_eq!(
            snapshot(&file, &hist, false).unwrap(),
            None,
            "within a minute"
        );
        let forced = snapshot(&file, &hist, true).unwrap().unwrap();
        assert!(forced > first);
        fs::write(hist.join("notes.json"), b"x").unwrap();
        fs::write(hist.join("12.json.tmp"), b"x").unwrap();
        fs::write(hist.join("-5.json"), b"x").unwrap();
        let ids: Vec<u64> = snapshots(&hist).iter().map(|s| s.0).collect();
        assert_eq!(ids, vec![forced, first]);
        for _ in 0..HISTORY_MAX + 5 {
            snapshot(&file, &hist, true).unwrap();
        }
        let list = snapshots(&hist);
        assert_eq!(list.len(), HISTORY_MAX);
        assert!(
            list.windows(2).all(|w| w[0].0 > w[1].0),
            "newest first, unique"
        );
        assert!(!list.iter().any(|s| s.0 == first), "the oldest went");
    }
}
