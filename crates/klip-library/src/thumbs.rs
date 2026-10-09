//! The thumbnail cache: `<folder>/.klip/thumbs/`, one poster (`<name>.jpg`) and one
//! hover-scrub strip (`<name>.strip.jpg`, [`STRIP_TILES`] frames stacked top to bottom)
//! per clip. `<name>` is the clip's key with `%` written as `%25` and `/` as `%2F`, so
//! every key has its own file. Everything here is a cache: losing it costs time, never
//! data.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::index::ms_since_epoch;
use crate::{Library, META_DIR, clean_rel};

/// Folder of the cache inside [`META_DIR`].
pub const THUMBS_DIR: &str = "thumbs";
/// Frames in a hover-scrub strip.
pub const STRIP_TILES: u32 = 16;
/// Width of a strip frame in pixels.
pub const STRIP_TILE_WIDTH: u32 = 240;
/// Width of a poster in pixels.
pub const POSTER_WIDTH: u32 = 320;
/// Largest thumbnail file served.
pub const MAX_THUMB_BYTES: u64 = 8 << 20;

const POSTER_SUFFIX: &str = ".jpg";
const STRIP_SUFFIX: &str = ".strip.jpg";

/// Which picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThumbKind {
    /// One frame near the end (the moment of the hotkey).
    Poster,
    /// [`STRIP_TILES`] frames spread over the clip, for hover-scrub.
    Strip,
}

impl ThumbKind {
    pub fn parse(s: &str) -> Option<ThumbKind> {
        match s {
            "poster" => Some(ThumbKind::Poster),
            "strip" => Some(ThumbKind::Strip),
            _ => None,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            ThumbKind::Poster => POSTER_SUFFIX,
            ThumbKind::Strip => STRIP_SUFFIX,
        }
    }
}

fn escape(key: &str) -> String {
    key.replace('%', "%25").replace('/', "%2F")
}

fn unescape(name: &str) -> String {
    name.replace("%2F", "/").replace("%25", "%")
}

/// The key and kind a cache file name belongs to.
fn parse_file_name(name: &str) -> Option<(String, ThumbKind)> {
    // The strip suffix also ends in the poster's: check it first.
    if let Some(stem) = name.strip_suffix(STRIP_SUFFIX) {
        return Some((unescape(stem), ThumbKind::Strip));
    }
    name.strip_suffix(POSTER_SUFFIX)
        .map(|stem| (unescape(stem), ThumbKind::Poster))
}

/// Thumbnail versions (modification time in ms) of one clip; `None`: not made yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ThumbVersions {
    pub poster: Option<u64>,
    pub strip: Option<u64>,
}

impl Library {
    pub fn thumbs_dir(&self) -> PathBuf {
        self.root.join(META_DIR).join(THUMBS_DIR)
    }

    /// Where a clip's thumbnail is (or goes); `None` for a key that isn't a clean
    /// relative path.
    pub fn thumb_path(&self, key: &str, kind: ThumbKind) -> Option<PathBuf> {
        clean_rel(key).then(|| {
            self.thumbs_dir()
                .join(format!("{}{}", escape(key), kind.suffix()))
        })
    }

    /// Every cached thumbnail by key (one directory listing).
    pub fn thumb_versions(&self) -> HashMap<String, ThumbVersions> {
        let mut out: HashMap<String, ThumbVersions> = HashMap::new();
        let Ok(entries) = fs::read_dir(self.thumbs_dir()) else {
            return out;
        };
        for e in entries.flatten().take(crate::MAX_CLIPS.saturating_mul(3)) {
            let name = e.file_name();
            let Some((key, kind)) = parse_file_name(&name.to_string_lossy()) else {
                continue;
            };
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() || meta.len() == 0 {
                continue;
            }
            let v = ms_since_epoch(meta.modified().unwrap_or(UNIX_EPOCH)).max(1);
            let slot = out.entry(key).or_default();
            match kind {
                ThumbKind::Poster => slot.poster = Some(v),
                ThumbKind::Strip => slot.strip = Some(v),
            }
        }
        out
    }

    /// Remove a clip's thumbnails (it was deleted).
    pub fn remove_thumbs(&self, key: &str) {
        for kind in [ThumbKind::Poster, ThumbKind::Strip] {
            if let Some(p) = self.thumb_path(key, kind) {
                let _ = fs::remove_file(p);
            }
        }
    }

    /// Move a clip's thumbnails to its new key (it was renamed).
    pub fn move_thumbs(&self, from: &str, to: &str) {
        for kind in [ThumbKind::Poster, ThumbKind::Strip] {
            if let (Some(a), Some(b)) = (self.thumb_path(from, kind), self.thumb_path(to, kind))
                && a.exists()
            {
                let _ = fs::remove_file(&b);
                let _ = fs::rename(&a, &b);
            }
        }
    }

    /// Delete cached thumbnails of clips that are gone, and leftover temp files.
    /// Returns how many files went.
    pub fn prune_thumbs(&self, keys: &HashSet<&str>) -> usize {
        let Ok(entries) = fs::read_dir(self.thumbs_dir()) else {
            return 0;
        };
        let mut removed = 0usize;
        for e in entries.flatten().take(crate::MAX_CLIPS.saturating_mul(4)) {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let stale = if name.ends_with(".tmp") {
                // A job that died half-way, at least an hour ago.
                e.metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > 3600)
            } else {
                match parse_file_name(&name) {
                    Some((key, _)) => !keys.contains(key.as_str()),
                    None => false,
                }
            };
            if stale && fs::remove_file(e.path()).is_ok() {
                removed = removed.saturating_add(1);
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_round_trip_and_never_collide() {
        for key in [
            "a.mp4",
            "Game/a.mp4",
            "Game%2Fa.mp4",
            "a~b.mp4",
            "100%/x.mp4",
        ] {
            for kind in [ThumbKind::Poster, ThumbKind::Strip] {
                let name = format!("{}{}", escape(key), kind.suffix());
                assert!(!name.contains('/'));
                assert_eq!(parse_file_name(&name), Some((key.to_string(), kind)));
            }
        }
        assert_ne!(escape("Game/a.mp4"), escape("Game%2Fa.mp4"));
        assert!(parse_file_name("notes.txt").is_none());
    }
}
