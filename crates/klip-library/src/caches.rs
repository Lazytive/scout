//! Per-clip files beside the thumbnails (ADR 0017): the editor's project
//! (`.klip/projects/<key>.json`: the user's edits, kept as long as the clip exists),
//! the H.264 preview proxy (`.klip/proxies/<key>.mp4`) and rendered text bitmaps
//! (`.klip/render/<key>/`). Keys are escaped like thumbnails. They follow a clip's
//! rename and delete; `prune_caches` removes what belongs to clips that are gone.
//! Also the name of an export next to its clip (`unique_sibling`).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::names::{MAX_NAME_CHARS, check_clip_name};
use crate::thumbs::{escape, unescape};
use crate::{Library, LibraryError, META_DIR, clean_rel, with_suffix};

/// Folder under `.klip` with the editor projects.
pub const PROJECTS_DIR: &str = "projects";
/// Folder under `.klip` with the preview proxies.
pub const PROXIES_DIR: &str = "proxies";
/// Folder under `.klip` with rendered text bitmaps, one folder per clip.
pub const RENDER_DIR: &str = "render";
const PROJECT_SUFFIX: &str = ".json";
const PROXY_SUFFIX: &str = ".mp4";
/// Most ` (n)` suffixes tried for a free export name.
const MAX_UNIQUE: u32 = 1000;

/// The three caches: folder, file suffix (`None`: one folder per key).
const KINDS: [(&str, Option<&str>); 3] = [
    (PROJECTS_DIR, Some(PROJECT_SUFFIX)),
    (PROXIES_DIR, Some(PROXY_SUFFIX)),
    (RENDER_DIR, None),
];

impl Library {
    fn cache_dir(&self, sub: &str) -> PathBuf {
        self.root().join(META_DIR).join(sub)
    }

    fn cache_path(&self, sub: &str, suffix: Option<&str>, key: &str) -> Option<PathBuf> {
        clean_rel(key).then(|| {
            self.cache_dir(sub)
                .join(format!("{}{}", escape(key), suffix.unwrap_or("")))
        })
    }

    /// Where a clip's editor project is (or goes); `None` for a dirty key.
    pub fn project_path(&self, key: &str) -> Option<PathBuf> {
        self.cache_path(PROJECTS_DIR, Some(PROJECT_SUFFIX), key)
    }

    /// Where a clip's preview proxy is (or goes).
    pub fn proxy_path(&self, key: &str) -> Option<PathBuf> {
        self.cache_path(PROXIES_DIR, Some(PROXY_SUFFIX), key)
    }

    /// The folder for a clip's rendered text bitmaps.
    pub fn render_dir(&self, key: &str) -> Option<PathBuf> {
        self.cache_path(RENDER_DIR, None, key)
    }

    /// Move a clip's project, proxy and renders to its new key (it was renamed).
    pub fn move_caches(&self, from: &str, to: &str) {
        for (sub, suffix) in KINDS {
            if let (Some(a), Some(b)) = (
                self.cache_path(sub, suffix, from),
                self.cache_path(sub, suffix, to),
            ) && a.exists()
            {
                if suffix.is_some() {
                    let _ = fs::remove_file(&b);
                } else {
                    let _ = fs::remove_dir_all(&b);
                }
                let _ = fs::rename(&a, &b);
            }
        }
    }

    /// Remove a clip's project, proxy and renders (it was deleted).
    pub fn remove_caches(&self, key: &str) {
        for (sub, suffix) in KINDS {
            if let Some(p) = self.cache_path(sub, suffix, key) {
                if suffix.is_some() {
                    let _ = fs::remove_file(p);
                } else {
                    let _ = fs::remove_dir_all(p);
                }
            }
        }
    }

    /// Delete projects, proxies and renders of clips that are gone, and leftover temp
    /// files; only for a complete `keys` set (a partial scan must not lose projects).
    /// Returns how many entries went.
    pub fn prune_caches(&self, keys: &HashSet<&str>) -> usize {
        let mut removed = 0usize;
        for (sub, suffix) in KINDS {
            let Ok(entries) = fs::read_dir(self.cache_dir(sub)) else {
                continue;
            };
            for e in entries.flatten().take(crate::MAX_CLIPS.saturating_mul(2)) {
                let name = e.file_name();
                let name = name.to_string_lossy();
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                let stale = if name.ends_with(".tmp") || name.ends_with(".part") {
                    e.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_secs() > 3600)
                } else {
                    let key = match suffix {
                        Some(s) if !is_dir => name.strip_suffix(s).map(unescape),
                        None if is_dir => Some(unescape(&name)),
                        _ => None,
                    };
                    key.is_some_and(|k| !keys.contains(k.as_str()))
                };
                if !stale {
                    continue;
                }
                let gone = if is_dir {
                    fs::remove_dir_all(e.path()).is_ok()
                } else {
                    fs::remove_file(e.path()).is_ok()
                };
                if gone {
                    removed = removed.saturating_add(1);
                }
            }
        }
        removed
    }

    /// A free name next to `clip` for a derived file: `<name> - <suffix>.mp4`, then
    /// `<name> - <suffix> (2).mp4` and so on. The name is shortened so the whole stays a
    /// valid clip name. Never an existing file (or one being written, `.part`).
    pub fn unique_sibling(&self, clip: &Path, suffix: &str) -> Result<PathBuf, LibraryError> {
        self.key(clip)?;
        let stem = clip
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .ok_or(LibraryError::NoClip)?;
        let suffix = check_clip_name(suffix).map_err(LibraryError::BadName)?;
        let dir = clip.parent().ok_or(LibraryError::NoClip)?;
        // "<stem> - <suffix> (1000)" must fit MAX_NAME_CHARS.
        let room = MAX_NAME_CHARS.saturating_sub(suffix.chars().count().saturating_add(10));
        let stem: String = stem.trim().chars().take(room.max(1)).collect();
        let stem = check_clip_name(&stem).map_err(LibraryError::BadName)?;
        for n in 1..=MAX_UNIQUE {
            let name = if n == 1 {
                format!("{stem} - {suffix}")
            } else {
                format!("{stem} - {suffix} ({n})")
            };
            let p = dir.join(format!("{name}.mp4"));
            if !p.exists() && !with_suffix(&p, ".part").exists() {
                return Ok(p);
            }
        }
        Err(LibraryError::Exists(format!("{stem} - {suffix}.mp4")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::new(dir.path().to_path_buf());
        (dir, lib)
    }

    #[test]
    fn paths_escape_keys_and_reject_dirty_ones() {
        let (_d, lib) = lib();
        let p = lib.project_path("Game/a%b.mp4").unwrap();
        assert!(p.ends_with("projects/Game%2Fa%25b.mp4.json"));
        assert!(
            lib.proxy_path("a.mp4")
                .unwrap()
                .ends_with("proxies/a.mp4.mp4")
        );
        assert!(lib.render_dir("a.mp4").unwrap().ends_with("render/a.mp4"));
        assert!(lib.project_path("../a.mp4").is_none());
        assert!(lib.project_path("").is_none());
    }

    #[test]
    fn caches_follow_rename_delete_and_prune() {
        let (_d, lib) = lib();
        let write = |p: PathBuf| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"x").unwrap();
            p
        };
        let proj = write(lib.project_path("a.mp4").unwrap());
        let proxy = write(lib.proxy_path("a.mp4").unwrap());
        let render = lib.render_dir("a.mp4").unwrap();
        write(render.join("1.png"));
        lib.move_caches("a.mp4", "Game/b.mp4");
        assert!(!proj.exists() && !proxy.exists() && !render.exists());
        assert!(lib.project_path("Game/b.mp4").unwrap().is_file());
        assert!(lib.proxy_path("Game/b.mp4").unwrap().is_file());
        assert!(
            lib.render_dir("Game/b.mp4")
                .unwrap()
                .join("1.png")
                .is_file()
        );
        // Prune keeps what belongs to a known clip, drops the rest.
        write(lib.project_path("gone.mp4").unwrap());
        write(lib.render_dir("gone.mp4").unwrap().join("x.png"));
        let keys: HashSet<&str> = ["Game/b.mp4"].into_iter().collect();
        assert_eq!(lib.prune_caches(&keys), 2);
        assert!(lib.project_path("Game/b.mp4").unwrap().is_file());
        assert!(!lib.project_path("gone.mp4").unwrap().exists());
        lib.remove_caches("Game/b.mp4");
        assert!(!lib.project_path("Game/b.mp4").unwrap().exists());
        assert!(!lib.render_dir("Game/b.mp4").unwrap().exists());
        // Unknown keys and missing folders are fine.
        lib.remove_caches("../x");
        lib.move_caches("nope.mp4", "also.mp4");
    }

    #[test]
    fn unique_sibling_counts_up_and_stays_a_clip_name() {
        let (d, lib) = lib();
        let clip = d.path().join("Game").join("My clip.mp4");
        fs::create_dir_all(clip.parent().unwrap()).unwrap();
        fs::write(&clip, b"x").unwrap();
        let first = lib.unique_sibling(&clip, "Discord").unwrap();
        assert_eq!(first, clip.with_file_name("My clip - Discord.mp4"));
        fs::write(&first, b"x").unwrap();
        fs::write(clip.with_file_name("My clip - Discord (2).mp4.part"), b"x").unwrap();
        let third = lib.unique_sibling(&clip, "Discord").unwrap();
        assert_eq!(third, clip.with_file_name("My clip - Discord (3).mp4"));
        let long = d.path().join(format!("{}.mp4", "n".repeat(119)));
        fs::write(&long, b"x").unwrap();
        let p = lib.unique_sibling(&long, "YouTube").unwrap();
        let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(stem.chars().count() <= MAX_NAME_CHARS, "{stem}");
        assert!(stem.ends_with(" - YouTube"));
        assert!(lib.unique_sibling(&clip, "a/b").is_err());
        assert!(
            lib.unique_sibling(Path::new("C:/elsewhere/x.mp4"), "Discord")
                .is_err()
        );
    }
}
