//! The clip folder (ADR 0014, ADR 0015): Scout's metadata about it (stars, tags), a scan
//! of the clips in it, renaming, the info cache and thumbnail cache, searching and
//! filtering for the library grid, and the storage-cap policy.
//!
//! Clips stay plain MP4 files in a normal folder. The metadata lives in
//! `<folder>/.klip/library.json`; losing it loses stars and tags, never clips. The info
//! index and thumbnails next to it are caches, rebuilt from the clips. Pure: no OS calls
//! besides reading and writing files (moving to the Recycle Bin is `klip-platform`'s).

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

mod caches;
mod index;
mod names;
mod query;
mod thumbs;

pub use caches::{AUDIO_DIR, HISTORY_DIR, PROJECTS_DIR, PROXIES_DIR, RENDER_DIR};
pub use index::{
    ClipEvent, ClipInfo, INDEX_FILE, Index, IndexEntry, audio_track_names, probe_file,
};
pub use names::{
    MAX_NAME_CHARS, MAX_TAG_CHARS, MAX_TAGS, check_clip_name, clean_tag, normalize_tags,
};
pub use query::{Facet, ListedClip, Listing, MAX_LIST, Query, Sort, game_of, query};
pub use thumbs::{
    MAX_THUMB_BYTES, POSTER_WIDTH, STRIP_TILE_WIDTH, STRIP_TILES, THUMBS_DIR, ThumbKind,
    ThumbVersions,
};

/// Metadata folder inside the clip folder.
pub const META_DIR: &str = ".klip";
/// Metadata file inside [`META_DIR`].
pub const META_FILE: &str = "library.json";
/// Metadata files bigger than this are not read.
pub const MAX_META_BYTES: u64 = 16 << 20;
/// Most clips scanned (and most metadata entries kept).
pub const MAX_CLIPS: usize = 200_000;
/// How deep the scan goes: the folder and one level of game folders below it.
pub const MAX_DEPTH: usize = 2;
/// Suffix of a full-session recording that is still being written (ADR 0013).
pub const RECORDING_SIDECAR: &str = ".recording.json";
/// One GiB, the unit of the storage settings.
pub const GIB: u64 = 1 << 30;

/// What Scout knows about one clip besides the file itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipMeta {
    /// Starred clips are never removed to stay under the cap.
    pub starred: bool,
    /// The user's tags ([`normalize_tags`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// `library.json`. Keys are paths relative to the clip folder with `/` separators.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub version: u32,
    pub clips: BTreeMap<String, ClipMeta>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            version: 1,
            clips: BTreeMap::new(),
        }
    }
}

/// Errors from writing metadata.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is not inside the clip folder")]
    Outside(String),
    #[error("{0}")]
    BadName(String),
    #[error("there is already a clip called {0}")]
    Exists(String),
    #[error("the clip is still being recorded")]
    Recording,
    #[error("no such clip")]
    NoClip,
}

pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(path.as_os_str());
    s.push(suffix);
    PathBuf::from(s)
}

/// Write `data` via a temp file, fsync and rename.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = with_suffix(path, ".tmp");
    let res = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// The clip folder and its metadata.
#[derive(Clone, Debug)]
pub struct Library {
    pub(crate) root: PathBuf,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Library {
        Library { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn meta_path(&self) -> PathBuf {
        self.root.join(META_DIR).join(META_FILE)
    }

    /// The metadata; empty when missing. A file that is too big or not valid is moved
    /// aside to `library.json.bad` (stars in it are lost, clips are not).
    pub fn load_meta(&self) -> Meta {
        let path = self.meta_path();
        let Ok(f) = File::open(&path) else {
            return Meta::default();
        };
        let mut bytes = Vec::new();
        let read = f
            .take(MAX_META_BYTES.saturating_add(1))
            .read_to_end(&mut bytes);
        let parsed = match read {
            Ok(n) if u64::try_from(n).unwrap_or(u64::MAX) <= MAX_META_BYTES => {
                serde_json::from_slice::<Meta>(&bytes).ok()
            }
            _ => None,
        };
        match parsed {
            Some(mut m) => {
                // Hostile files: cap the entries, drop keys that aren't clean relative paths.
                m.clips.retain(|k, _| clean_rel(k));
                while m.clips.len() > MAX_CLIPS {
                    m.clips.pop_last();
                }
                for c in m.clips.values_mut() {
                    if !c.tags.is_empty() {
                        c.tags = names::lenient_tags(c.tags.iter().map(String::as_str), MAX_TAGS);
                    }
                }
                m
            }
            None => {
                tracing::warn!("clip metadata unreadable; starting over (kept as .bad)");
                let _ = fs::rename(&path, with_suffix(&path, ".bad"));
                Meta::default()
            }
        }
    }

    pub fn save_meta(&self, meta: &Meta) -> Result<(), LibraryError> {
        let text = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
        write_atomic(&self.meta_path(), &text)?;
        Ok(())
    }

    /// The metadata key of a clip path (relative, `/` separators).
    pub fn key(&self, path: &Path) -> Result<String, LibraryError> {
        let rel = path
            .strip_prefix(&self.root)
            .map_err(|_| LibraryError::Outside(path.to_string_lossy().into_owned()))?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let key = parts.join("/");
        if clean_rel(&key) {
            Ok(key)
        } else {
            Err(LibraryError::Outside(path.to_string_lossy().into_owned()))
        }
    }

    /// Star or unstar a clip.
    pub fn set_starred(&self, path: &Path, starred: bool) -> Result<(), LibraryError> {
        let key = self.key(path)?;
        let mut meta = self.load_meta();
        if starred {
            meta.clips.entry(key).or_default().starred = true;
        } else if let Some(m) = meta.clips.get_mut(&key) {
            m.starred = false;
            if *m == ClipMeta::default() {
                meta.clips.remove(&key);
            }
        }
        self.save_meta(&meta)
    }

    /// Set a clip's tags (as typed; stored normalised, [`normalize_tags`]). Returns them.
    pub fn set_tags(&self, path: &Path, tags: &[String]) -> Result<Vec<String>, LibraryError> {
        let key = self.key(path)?;
        let tags = normalize_tags(tags).map_err(LibraryError::BadName)?;
        let mut meta = self.load_meta();
        let entry = meta.clips.entry(key.clone()).or_default();
        entry.tags.clone_from(&tags);
        if *entry == ClipMeta::default() {
            meta.clips.remove(&key);
        }
        self.save_meta(&meta)?;
        Ok(tags)
    }

    /// Rename a clip file to `name` (+ `.mp4`, [`check_clip_name`]) in its folder; its
    /// stars, tags and thumbnails follow. Never replaces another file. Returns the new
    /// path.
    pub fn rename(&self, path: &Path, name: &str) -> Result<PathBuf, LibraryError> {
        let old_key = self.key(path)?;
        if !path.is_file() {
            return Err(LibraryError::NoClip);
        }
        if with_suffix(path, RECORDING_SIDECAR).exists() {
            return Err(LibraryError::Recording);
        }
        let name = check_clip_name(name).map_err(LibraryError::BadName)?;
        let dir = path.parent().ok_or(LibraryError::NoClip)?;
        let new_path = dir.join(format!("{name}.mp4"));
        if new_path == path {
            return Ok(new_path);
        }
        if new_path.exists() && !same_file(path, &new_path) {
            return Err(LibraryError::Exists(format!("{name}.mp4")));
        }
        let new_key = self.key(&new_path)?;
        fs::rename(path, &new_path)?;
        let mut meta = self.load_meta();
        if let Some(m) = meta.clips.remove(&old_key) {
            meta.clips.insert(new_key.clone(), m);
            if let Err(e) = self.save_meta(&meta) {
                tracing::warn!(error = %e, "renamed a clip but could not move its stars/tags");
            }
        }
        self.move_thumbs(&old_key, &new_key);
        self.move_caches(&old_key, &new_key);
        Ok(new_path)
    }

    /// Every finished clip (MP4) in the folder and its game folders: recordings still
    /// being written (with a sidecar), temp files and the metadata folder are skipped.
    pub fn scan(&self) -> Vec<ClipFile> {
        let mut out = Vec::new();
        self.scan_dir(&self.root, 1, &mut out);
        out
    }

    fn scan_dir(&self, dir: &Path, depth: usize, out: &mut Vec<ClipFile>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_CLIPS {
                return;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                if depth < MAX_DEPTH && !name.starts_with('.') {
                    self.scan_dir(&path, depth.saturating_add(1), out);
                }
                continue;
            }
            if !ft.is_file() || !name.to_ascii_lowercase().ends_with(".mp4") {
                continue;
            }
            if with_suffix(&path, RECORDING_SIDECAR).exists() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(key) = self.key(&path) else { continue };
            out.push(ClipFile {
                key,
                path,
                bytes: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
}

/// Whether `a` and `b` are the same file (a case-only rename on a case-insensitive
/// file system).
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Whether `key` is a relative path with no `..`, no empty parts and no backslashes.
pub(crate) fn clean_rel(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 4096
        && !key.contains('\\')
        && !key.starts_with('/')
        && !key.contains(':')
        && key
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// A clip on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipFile {
    pub key: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
}

/// Totals for the storage settings page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub clips: usize,
    pub bytes: u64,
    pub starred: usize,
    pub starred_bytes: u64,
}

pub fn usage(files: &[ClipFile], meta: &Meta) -> Usage {
    let mut u = Usage::default();
    for f in files {
        u.clips = u.clips.saturating_add(1);
        u.bytes = u.bytes.saturating_add(f.bytes);
        if meta.clips.get(&f.key).is_some_and(|m| m.starred) {
            u.starred = u.starred.saturating_add(1);
            u.starred_bytes = u.starred_bytes.saturating_add(f.bytes);
        }
    }
    u
}

/// The clips to remove so the folder fits `cap_bytes` (0: no cap): the oldest unstarred
/// ones first, never a starred or a protected one (e.g. the clip just saved). When the
/// starred and protected clips alone are over the cap, all others go and the folder
/// stays over it.
pub fn over_cap(
    files: &[ClipFile],
    meta: &Meta,
    cap_bytes: u64,
    protect: &[PathBuf],
) -> Vec<ClipFile> {
    if cap_bytes == 0 {
        return Vec::new();
    }
    let total = files.iter().fold(0u64, |t, f| t.saturating_add(f.bytes));
    if total <= cap_bytes {
        return Vec::new();
    }
    let mut candidates: Vec<&ClipFile> = files
        .iter()
        .filter(|f| !meta.clips.get(&f.key).is_some_and(|m| m.starred))
        .filter(|f| !protect.iter().any(|p| p == &f.path))
        .collect();
    candidates.sort_by(|a, b| a.modified.cmp(&b.modified).then(a.key.cmp(&b.key)));
    let mut left = total;
    let mut out = Vec::new();
    for f in candidates {
        if left <= cap_bytes {
            break;
        }
        left = left.saturating_sub(f.bytes);
        out.push(f.clone());
    }
    out
}

/// Forget the metadata of clips Scout removed. (Entries of clips deleted elsewhere stay:
/// a missing file may be on a drive that is just unplugged.)
pub fn forget(meta: &mut Meta, keys: &[String]) -> bool {
    let before = meta.clips.len();
    for k in keys {
        meta.clips.remove(k);
    }
    meta.clips.len() != before
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod library_tests;
