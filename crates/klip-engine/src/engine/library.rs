//! The library (ADR 0015): `library.*` and `clip.*` commands over the clip folder, the
//! info index (`klip_library::Index`, kept in memory and in `.klip/index.json`) and the
//! thumbnail queue. A `klip-thumbs` thread makes missing posters and hover-scrub strips
//! through the media worker (a child process): first the clips the UI asks for (visible
//! cards), then, while no game is being recorded, every other clip, newest first. A clip
//! whose thumbnails failed is not tried again until its file changes.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use klip_control::CommandError;
use klip_library::{
    ClipInfo, Index, Library, LibraryError, MAX_THUMB_BYTES, POSTER_WIDTH, Query, STRIP_TILE_WIDTH,
    STRIP_TILES, ThumbKind, ThumbVersions,
};
use serde_json::{Value, json};

use super::{Inner, lock};
use crate::config;
use crate::events::Event;
use crate::media_worker::{MediaWorker, ThumbJob};

/// How long one thumbnail job may take (opening any clip and decoding 17 frames).
const JOB_TIMEOUT: Duration = Duration::from_secs(60);
/// Most keys one `library.thumbs` call queues, and most waiting urgent keys.
const MAX_URGENT: usize = 256;
/// Most paths one `clip.delete` call takes.
const MAX_DELETE: usize = 10_000;
/// How long the thumbnail thread sleeps when there is nothing to do.
const IDLE_WAIT: Duration = Duration::from_secs(5);

/// The in-memory index and the folder it belongs to.
#[derive(Default)]
pub(super) struct LibState {
    root: Option<PathBuf>,
    index: Index,
    /// Orphaned thumbnails were cleaned up for this folder.
    pruned: bool,
}

impl LibState {
    pub(super) fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The indexed info of `key`, if it was read.
    pub(super) fn info_of(&self, key: &str) -> Option<ClipInfo> {
        self.index.clips.get(key).and_then(|e| e.info.clone())
    }
}

/// Clips waiting for thumbnails.
#[derive(Default)]
pub(super) struct ThumbQueue {
    root: Option<PathBuf>,
    /// Asked for by the UI, in the order to make them.
    urgent: VecDeque<String>,
    /// Every other clip missing thumbnails, newest first; only made while no game runs.
    background: VecDeque<String>,
    started: bool,
    next_id: u64,
}

fn lib_err(e: LibraryError) -> CommandError {
    match e {
        LibraryError::Io(e) => CommandError::new("failed", e.to_string()),
        other => CommandError::bad_args(other.to_string()),
    }
}

pub(super) fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, CommandError> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| CommandError::bad_args(format!("{name} must be a string")))
}

/// What the file of `key` looks like now: versions of both thumbnails (one stat each).
fn versions_of(lib: &Library, key: &str) -> ThumbVersions {
    let v = |kind| {
        let p = lib.thumb_path(key, kind)?;
        let m = std::fs::metadata(p).ok().filter(|m| m.len() > 0)?;
        let t = m.modified().ok()?;
        let ms = t
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())?;
        Some(ms.max(1))
    };
    ThumbVersions {
        poster: v(ThumbKind::Poster),
        strip: v(ThumbKind::Strip),
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn clip_path(lib: &Library, key: &str) -> PathBuf {
    key.split('/')
        .fold(lib.root().to_path_buf(), |p, part| p.join(part))
}

impl Inner {
    pub(super) fn clip_library(&self) -> Result<Library, CommandError> {
        config::clip_base(&self.settings())
            .map(Library::new)
            .map_err(|e| CommandError::new("failed", e.to_string()))
    }

    /// The clip named by `args.path`: inside the clip folder and an existing file.
    pub(super) fn clip_arg(
        &self,
        lib: &Library,
        path: &str,
    ) -> Result<(PathBuf, String), CommandError> {
        let p = PathBuf::from(path);
        let key = lib.key(&p).map_err(lib_err)?;
        if !p.is_file() {
            return Err(CommandError::bad_args("no such clip"));
        }
        Ok((p, key))
    }

    /// `library.list`: the clips matching a query (all of them by default), with counts
    /// for the filters. Reads new and changed clips into the index first.
    pub(super) fn library_list(&self, args: &Value) -> Result<Value, CommandError> {
        let q: Query = if args.is_null() {
            Query::default()
        } else {
            serde_json::from_value(args.clone())
                .map_err(|e| CommandError::bad_args(e.to_string()))?
        };
        let lib = self.clip_library()?;
        let (files, meta) = {
            let _guard = lock(&self.library_lock);
            (lib.scan(), lib.load_meta())
        };
        let mut state = lock(&self.library);
        if state.root.as_deref() != Some(lib.root()) {
            *state = LibState {
                root: Some(lib.root().to_path_buf()),
                index: lib.load_index(),
                pruned: false,
            };
        }
        if lib.refresh_index(&files, &mut state.index)
            && let Err(e) = lib.save_index(&state.index)
        {
            tracing::warn!(error = %e, "could not save the library index");
        }
        if !state.pruned && files.len() < klip_library::MAX_CLIPS {
            let keys: HashSet<&str> = files.iter().map(|f| f.key.as_str()).collect();
            let n = lib
                .prune_thumbs(&keys)
                .saturating_add(lib.prune_caches(&keys));
            if n > 0 {
                tracing::info!(removed = n, "orphaned thumbnails, projects and proxies");
            }
            state.pruned = true;
        }
        let thumbs = lib.thumb_versions();
        let now = now_ms();
        let listing = klip_library::query(&files, &meta, &state.index, &thumbs, &q, now);
        // Everything still missing a thumbnail, newest first, for the background queue.
        let mut missing: Vec<&klip_library::ClipFile> = files
            .iter()
            .filter(|f| {
                let t = thumbs.get(&f.key).copied().unwrap_or_default();
                let failed = state
                    .index
                    .clips
                    .get(&f.key)
                    .is_some_and(|e| e.thumbs_failed);
                (t.poster.is_none() || t.strip.is_none()) && !failed
            })
            .collect();
        missing.sort_by_key(|f| std::cmp::Reverse(f.modified));
        let missing: Vec<String> = missing.into_iter().map(|f| f.key.clone()).collect();
        drop(state);
        let pending = missing.len();
        self.queue_thumbs(lib.root(), None, Some(missing));
        let mut v = serde_json::to_value(&listing)
            .map_err(|e| CommandError::new("failed", e.to_string()))?;
        if let Some(o) = v.as_object_mut() {
            o.insert("folder".into(), json!(lib.root().to_string_lossy()));
            o.insert(
                "strip".into(),
                json!({ "tiles": STRIP_TILES, "tile_width": STRIP_TILE_WIDTH }),
            );
            o.insert("thumbs_pending".into(), json!(pending));
        }
        Ok(v)
    }

    /// `library.thumbs`: make these clips' thumbnails next (the cards on screen).
    pub(super) fn library_thumbs(&self, args: &Value) -> Result<Value, CommandError> {
        let keys = args
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| CommandError::bad_args("keys must be a list"))?;
        if keys.len() > MAX_URGENT {
            return Err(CommandError::bad_args(format!(
                "at most {MAX_URGENT} keys at a time"
            )));
        }
        let lib = self.clip_library()?;
        let mut wanted = Vec::new();
        for k in keys {
            let k = k
                .as_str()
                .ok_or_else(|| CommandError::bad_args("keys must be strings"))?;
            if lib.thumb_path(k, ThumbKind::Poster).is_none() {
                return Err(CommandError::bad_args("not a clip key"));
            }
            let v = versions_of(&lib, k);
            if (v.poster.is_none() || v.strip.is_none()) && clip_path(&lib, k).is_file() {
                wanted.push(k.to_string());
            }
        }
        let queued = wanted.len();
        self.queue_thumbs(lib.root(), Some(wanted), None);
        Ok(json!({ "queued": queued }))
    }

    /// `library.thumb`: the cached thumbnail file of a clip (for the app's `klipthumb`
    /// URI scheme), or `not_found`.
    pub(super) fn library_thumb(&self, args: &Value) -> Result<Value, CommandError> {
        let key = str_arg(args, "key")?;
        let kind = ThumbKind::parse(str_arg(args, "kind")?)
            .ok_or_else(|| CommandError::bad_args("kind must be poster or strip"))?;
        let lib = self.clip_library()?;
        let path = lib
            .thumb_path(key, kind)
            .ok_or_else(|| CommandError::bad_args("not a clip key"))?;
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() > 0 && m.len() <= MAX_THUMB_BYTES => {
                Ok(json!({ "path": path.to_string_lossy(), "bytes": m.len() }))
            }
            _ => Err(CommandError::new("not_found", "no thumbnail yet")),
        }
    }

    pub(super) fn clip_rename(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let name = str_arg(args, "name")?;
        let (path, old_key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let new_path = {
            let _guard = lock(&self.library_lock);
            lib.rename(&path, name).map_err(lib_err)?
        };
        let new_key = lib.key(&new_path).map_err(lib_err)?;
        {
            let mut state = lock(&self.library);
            if state.root.as_deref() == Some(lib.root())
                && let Some(e) = state.index.clips.remove(&old_key)
            {
                state.index.clips.insert(new_key.clone(), e);
                if let Err(e) = lib.save_index(&state.index) {
                    tracing::warn!(error = %e, "could not save the library index");
                }
            }
        }
        self.emit(&Event::LibraryChanged {
            reason: "renamed".into(),
        });
        let stem = new_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(json!({ "path": new_path.to_string_lossy(), "key": new_key, "name": stem }))
    }

    pub(super) fn clip_tags(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, _) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let tags: Vec<String> = args
            .get("tags")
            .and_then(Value::as_array)
            .ok_or_else(|| CommandError::bad_args("tags must be a list"))?
            .iter()
            .map(|t| {
                t.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| CommandError::bad_args("tags must be strings"))
            })
            .collect::<Result<_, _>>()?;
        let tags = {
            let _guard = lock(&self.library_lock);
            lib.set_tags(&path, &tags).map_err(lib_err)?
        };
        self.emit(&Event::LibraryChanged {
            reason: "tags".into(),
        });
        Ok(json!({ "tags": tags }))
    }

    /// `clip.delete`: move clips to the Recycle Bin / Trash (`permanent: true`: delete
    /// them, like Shift+Delete in a file manager), and forget their stars, tags, index
    /// entries and thumbnails.
    pub(super) fn clip_delete(&self, args: &Value) -> Result<Value, CommandError> {
        let paths = args
            .get("paths")
            .and_then(Value::as_array)
            .ok_or_else(|| CommandError::bad_args("paths must be a list"))?;
        if paths.len() > MAX_DELETE {
            return Err(CommandError::bad_args(format!(
                "at most {MAX_DELETE} clips at a time"
            )));
        }
        let permanent = match args.get("permanent") {
            None | Some(Value::Null) => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| CommandError::bad_args("permanent must be true or false"))?,
        };
        let lib = self.clip_library()?;
        let mut failed = Vec::new();
        let mut doomed: Vec<(PathBuf, String, u64)> = Vec::new();
        for p in paths {
            let Some(s) = p.as_str() else {
                return Err(CommandError::bad_args("paths must be strings"));
            };
            match self.clip_arg(&lib, s) {
                Ok((path, key)) => {
                    let mut sidecar = path.as_os_str().to_owned();
                    sidecar.push(klip_library::RECORDING_SIDECAR);
                    if Path::new(&sidecar).exists() {
                        failed.push(json!({ "path": s, "error": "still being recorded" }));
                        continue;
                    }
                    let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                    doomed.push((path, key, bytes));
                }
                Err(e) => failed.push(json!({ "path": s, "error": e.message })),
            }
        }
        let mut gone = Vec::new();
        let mut bytes = 0u64;
        {
            let _guard = lock(&self.library_lock);
            let list: Vec<PathBuf> = doomed.iter().map(|d| d.0.clone()).collect();
            let results: Vec<Result<(), String>> = if list.is_empty() {
                Vec::new()
            } else if permanent {
                list.iter()
                    .map(|p| std::fs::remove_file(p).map_err(|e| e.to_string()))
                    .collect()
            } else {
                klip_platform::move_to_trash(&list)
                    .into_iter()
                    .map(|r| r.map_err(|e| e.to_string()))
                    .collect()
            };
            for ((path, key, b), r) in doomed.iter().zip(results) {
                match r {
                    Ok(()) => {
                        gone.push(key.clone());
                        bytes = bytes.saturating_add(*b);
                        lib.remove_thumbs(key);
                        lib.remove_caches(key);
                    }
                    Err(e) => failed.push(json!({
                        "path": path.to_string_lossy(),
                        "error": e,
                    })),
                }
            }
            let mut meta = lib.load_meta();
            if klip_library::forget(&mut meta, &gone)
                && let Err(e) = lib.save_meta(&meta)
            {
                tracing::warn!(error = %e, "could not save the clip metadata");
            }
        }
        {
            let mut state = lock(&self.library);
            if state.root.as_deref() == Some(lib.root()) {
                for k in &gone {
                    state.index.clips.remove(k);
                }
                if !gone.is_empty()
                    && let Err(e) = lib.save_index(&state.index)
                {
                    tracing::warn!(error = %e, "could not save the library index");
                }
            }
        }
        if !gone.is_empty() {
            self.emit(&Event::LibraryChanged {
                reason: "deleted".into(),
            });
        }
        Ok(json!({
            "removed": gone.len(),
            "bytes": bytes,
            "to_trash": !permanent,
            "failed": failed,
        }))
    }

    pub(super) fn clip_open(&self, args: &Value, reveal: bool) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, _) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let r = if reveal {
            klip_platform::reveal_file(&path)
        } else {
            klip_platform::open_file(&path)
        };
        r.map_err(|e| CommandError::new("failed", e.to_string()))?;
        Ok(json!({ "opened": true }))
    }

    /// Queue thumbnail work for the clips in `root`: `urgent` keys go first (in order),
    /// `background` replaces the background list. Starts the thumbnail thread if needed.
    pub(super) fn queue_thumbs(
        &self,
        root: &Path,
        urgent: Option<Vec<String>>,
        background: Option<Vec<String>>,
    ) {
        let start = {
            let mut q = lock(&self.thumbs.0);
            if q.root.as_deref() != Some(root) {
                q.root = Some(root.to_path_buf());
                q.urgent.clear();
                q.background.clear();
            }
            if let Some(keys) = urgent {
                let new: HashSet<&String> = keys.iter().collect();
                q.urgent.retain(|k| !new.contains(k));
                for k in keys.into_iter().rev() {
                    q.urgent.push_front(k);
                }
                q.urgent.truncate(MAX_URGENT);
            }
            if let Some(keys) = background {
                q.background = keys.into();
            }
            let start = !q.started;
            q.started = true;
            start
        };
        self.thumbs.1.notify_all();
        if start {
            self.start_thumbs_thread();
        }
    }

    fn start_thumbs_thread(&self) {
        let me = self.me.clone();
        let worker = match &self.media_worker {
            Some(exe) => MediaWorker::isolated(exe.clone()),
            None => MediaWorker::in_process(),
        };
        let spawned = std::thread::Builder::new()
            .name("klip-thumbs".into())
            .spawn(move || thumbs_loop(&me, worker));
        match spawned {
            Ok(h) => *lock(&self.thumbs_thread) = Some(h),
            Err(e) => {
                tracing::warn!(error = %e, "no thread for thumbnails");
                lock(&self.thumbs.0).started = false;
            }
        }
    }

    /// The next clip to make thumbnails for, waiting up to `wait`; `None` if there is
    /// none (or the engine stops).
    fn next_thumb_job(&self, wait: Duration) -> Option<(PathBuf, String, u64)> {
        let gaming = lock(&self.state).game.is_some();
        let mut q = lock(&self.thumbs.0);
        if q.urgent.is_empty() && (gaming || q.background.is_empty()) {
            q = match self.thumbs.1.wait_timeout(q, wait) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        if self.stop.load(Ordering::Relaxed) {
            return None;
        }
        let root = q.root.clone()?;
        let key = match q.urgent.pop_front() {
            Some(k) => k,
            None if !gaming => q.background.pop_front()?,
            None => return None,
        };
        // Done or already queued again later: the background list may repeat a key the
        // UI asked for.
        q.background.retain(|k| *k != key);
        q.next_id = q.next_id.wrapping_add(1);
        Some((root, key, q.next_id))
    }

    fn make_thumbs(&self, worker: &mut MediaWorker, root: PathBuf, key: &str, id: u64) {
        let lib = Library::new(root);
        let clip = clip_path(&lib, key);
        if !clip.is_file() {
            return;
        }
        let have = versions_of(&lib, key);
        if have.poster.is_some() && have.strip.is_some() {
            return;
        }
        let failed_before = {
            let state = lock(&self.library);
            state.root.as_deref() == Some(lib.root())
                && state.index.clips.get(key).is_some_and(|e| e.thumbs_failed)
        };
        if failed_before {
            return;
        }
        let job = ThumbJob {
            id,
            clip,
            poster: have
                .poster
                .is_none()
                .then(|| lib.thumb_path(key, ThumbKind::Poster))
                .flatten(),
            poster_width: POSTER_WIDTH,
            strip: have
                .strip
                .is_none()
                .then(|| lib.thumb_path(key, ThumbKind::Strip))
                .flatten(),
            tiles: STRIP_TILES,
            tile_width: STRIP_TILE_WIDTH,
        };
        let result = worker.run(&job, JOB_TIMEOUT);
        if let Err(e) = &result {
            tracing::warn!(error = %e, "thumbnails");
            let mut state = lock(&self.library);
            if state.root.as_deref() == Some(lib.root())
                && let Some(entry) = state.index.clips.get_mut(key)
            {
                entry.thumbs_failed = true;
                if let Err(e) = lib.save_index(&state.index) {
                    tracing::warn!(error = %e, "could not save the library index");
                }
            }
        }
        let v = versions_of(&lib, key);
        self.emit(&Event::ThumbsReady {
            key: key.to_string(),
            poster: v.poster,
            strip: v.strip,
            failed: result.is_err(),
        });
    }

    /// Make a just-saved clip's missing thumbnails first among the background work (they
    /// wait while the game runs).
    pub(super) fn queue_new_clip(&self, root: &Path, key: String) {
        {
            let mut q = lock(&self.thumbs.0);
            if q.root.as_deref() == Some(root) {
                q.background.retain(|k| *k != key);
                q.background.push_front(key);
                drop(q);
                self.thumbs.1.notify_all();
                return;
            }
        }
        self.queue_thumbs(root, None, Some(vec![key]));
    }

    /// Wake the thumbnail thread so it sees the engine stopping. It is not waited for: a
    /// job in flight ends by itself (the worker is a separate process with a timeout).
    pub(super) fn stop_thumbs(&self) {
        self.thumbs.1.notify_all();
        drop(lock(&self.thumbs_thread).take());
    }
}

fn thumbs_loop(me: &std::sync::Weak<Inner>, mut worker: MediaWorker) {
    loop {
        let Some(inner) = me.upgrade() else { break };
        if inner.stop.load(Ordering::Relaxed) {
            break;
        }
        match inner.next_thumb_job(IDLE_WAIT) {
            Some((root, key, id)) => {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    inner.make_thumbs(&mut worker, root, &key, id);
                }));
                if r.is_err() {
                    tracing::error!("thumbnail job panicked");
                }
            }
            None => worker.idle(),
        }
    }
}
