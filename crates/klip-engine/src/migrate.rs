//! The rename from Klip to Scout (ADR 0020): the user's data moves over once, at the
//! first start of Scout.
//!
//! - The settings file and the custom themes are **copied** from `<config>/Klip` to
//!   `<config>/Scout` (the old folder stays, so an old build still starts as it was).
//! - The default clip folder `Videos/Klip` (only when the settings don't name a folder)
//!   is **moved** to `Videos/Scout` with one directory rename: all or nothing, nothing is
//!   copied or deleted. When the rename can't be done (a file in it is open, `Videos/Scout`
//!   exists already, another drive…), Scout keeps using `Videos/Klip` by writing it into
//!   its settings as the clip folder. Either way every clip stays in the library.
//!
//! It runs only when `<config>/Scout/settings.json` doesn't exist yet and the Klip one
//! does, so a second start (or a new user) does nothing.

use std::fs;
use std::path::{Path, PathBuf};

/// The app's name: the folder of its settings, its clips and its caches.
pub const NAME: &str = "Scout";
/// The name before the rename.
pub const LEGACY_NAME: &str = "Klip";

/// Theme files copied at most (the engine lists 200 themes, each a JSON + a picture).
const MAX_THEME_FILES: usize = 1000;
/// Largest theme file copied (a picture is at most 8 MiB).
const MAX_THEME_FILE: u64 = 16 << 20;

/// Where to look: the per-user config folder and the Videos folder.
#[derive(Clone, Debug)]
pub struct Places {
    pub config: PathBuf,
    pub videos: Option<PathBuf>,
}

impl Places {
    /// The user's folders (`None` without a config folder).
    pub fn for_user() -> Option<Places> {
        Some(Places {
            config: dirs::config_dir()?,
            videos: dirs::video_dir().or_else(dirs::home_dir),
        })
    }
}

/// What happened to the default clip folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clips {
    /// Nothing to do: no `Videos/Klip`, or the settings name their own folder.
    Untouched,
    /// `Videos/Klip` is now `Videos/Scout`.
    Moved { to: PathBuf },
    /// Scout uses `Videos/Klip` (written into its settings) because it couldn't move.
    Kept { folder: PathBuf, why: String },
}

/// What the migration did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The Klip settings were copied (`false`: nothing to migrate).
    pub settings: bool,
    /// Custom theme files copied.
    pub theme_files: usize,
    pub clips: Clips,
    /// What went wrong on the way (the rest still ran).
    pub errors: Vec<String>,
}

impl Report {
    fn nothing() -> Report {
        Report {
            settings: false,
            theme_files: 0,
            clips: Clips::Untouched,
            errors: Vec::new(),
        }
    }
}

/// Copy `from` to `to` through a temp file and a rename.
fn copy_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let r = fs::copy(from, &tmp).and_then(|_| fs::rename(&tmp, to));
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

/// Copy the custom themes folder's files (flat, size-capped). Returns how many.
fn copy_themes(from: &Path, to: &Path, errors: &mut Vec<String>) -> usize {
    let Ok(rd) = fs::read_dir(from) else {
        return 0;
    };
    let mut n = 0usize;
    for e in rd.filter_map(Result::ok).take(MAX_THEME_FILES) {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() || meta.len() > MAX_THEME_FILE {
            continue;
        }
        if n == 0
            && let Err(err) = fs::create_dir_all(to)
        {
            errors.push(format!("creating the themes folder: {err}"));
            return 0;
        }
        match copy_atomic(&e.path(), &to.join(e.file_name())) {
            Ok(()) => n = n.saturating_add(1),
            Err(err) => errors.push(format!("copying a theme file: {err}")),
        }
    }
    n
}

/// Run the migration (see the module docs). Never fails: problems are in `errors`.
pub fn migrate(places: &Places) -> Report {
    let old_dir = places.config.join(LEGACY_NAME);
    let new_dir = places.config.join(NAME);
    let old_settings = old_dir.join("settings.json");
    let new_settings = new_dir.join("settings.json");
    if new_settings.exists() || !old_settings.is_file() {
        return Report::nothing();
    }
    let mut report = Report::nothing();
    if let Err(e) =
        fs::create_dir_all(&new_dir).and_then(|()| copy_atomic(&old_settings, &new_settings))
    {
        report.errors.push(format!("copying the settings: {e}"));
        return report;
    }
    report.settings = true;
    report.theme_files = copy_themes(
        &old_dir.join("themes"),
        &new_dir.join("themes"),
        &mut report.errors,
    );

    let mut loaded = klip_settings::load(&new_settings);
    if loaded.settings.clips.folder.is_some() {
        return report;
    }
    let Some(videos) = &places.videos else {
        return report;
    };
    let old_clips = videos.join(LEGACY_NAME);
    let new_clips = videos.join(NAME);
    if !old_clips.is_dir() {
        return report;
    }
    let why = if new_clips.exists() {
        format!("{} exists already", new_clips.display())
    } else {
        match fs::rename(&old_clips, &new_clips) {
            Ok(()) => {
                report.clips = Clips::Moved { to: new_clips };
                return report;
            }
            Err(e) => e.to_string(),
        }
    };
    // Keep the clips where they are: Scout's settings name the old folder.
    loaded.settings.clips.folder = Some(old_clips.to_string_lossy().into_owned());
    if let Err(e) = klip_settings::save(&new_settings, &loaded.settings) {
        report.errors.push(format!("saving the clip folder: {e}"));
    }
    report.clips = Clips::Kept {
        folder: old_clips,
        why,
    };
    report
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    struct Setup {
        _tmp: tempfile::TempDir,
        places: Places,
    }

    /// A Klip user: settings (with `extra` merged in), two themes, clips in Videos/Klip.
    fn klip_user(extra: serde_json::Value) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let videos = tmp.path().join("Videos");
        let old = config.join("Klip");
        fs::create_dir_all(old.join("themes")).unwrap();
        let mut s = serde_json::json!({ "hotkeys": [], "app": { "first_run_done": true } });
        if let (Some(s), Some(e)) = (s.as_object_mut(), extra.as_object()) {
            s.extend(e.clone());
        }
        fs::write(old.join("settings.json"), s.to_string()).unwrap();
        fs::write(old.join("themes").join("neon.json"), b"{}").unwrap();
        fs::write(old.join("themes").join("neon.png"), b"png").unwrap();
        let clips = videos.join("Klip");
        fs::create_dir_all(clips.join("Apex")).unwrap();
        fs::create_dir_all(clips.join(".klip").join("projects")).unwrap();
        fs::write(
            clips.join("Apex").join("Klip_2026-10-11_10-00-00.mp4"),
            b"clip",
        )
        .unwrap();
        fs::write(clips.join(".klip").join("projects").join("a.json"), b"{}").unwrap();
        Setup {
            _tmp: tmp,
            places: Places {
                config,
                videos: Some(videos),
            },
        }
    }

    fn settings(p: &Places) -> klip_settings::Settings {
        klip_settings::load(&p.config.join("Scout").join("settings.json")).settings
    }

    #[test]
    fn copies_settings_and_themes_and_moves_the_clip_folder() {
        let s = klip_user(serde_json::json!({}));
        let r = migrate(&s.places);
        let videos = s.places.videos.clone().unwrap();
        assert_eq!(
            r.clips,
            Clips::Moved {
                to: videos.join("Scout")
            }
        );
        assert!(r.settings && r.errors.is_empty(), "{r:?}");
        assert_eq!(r.theme_files, 2);
        let new = s.places.config.join("Scout");
        assert!(new.join("themes").join("neon.png").is_file());
        assert!(settings(&s.places).app.first_run_done);
        assert_eq!(settings(&s.places).clips.folder, None);
        // Moved whole, the library's own folder included; the Klip settings stay.
        assert_eq!(
            fs::read(
                videos
                    .join("Scout")
                    .join("Apex")
                    .join("Klip_2026-10-11_10-00-00.mp4")
            )
            .unwrap(),
            b"clip"
        );
        assert!(
            videos
                .join("Scout")
                .join(".klip")
                .join("projects")
                .join("a.json")
                .is_file()
        );
        assert!(!videos.join("Klip").exists());
        assert!(s.places.config.join("Klip").join("settings.json").is_file());
        // Once only.
        assert_eq!(migrate(&s.places), Report::nothing());
    }

    #[test]
    fn keeps_the_old_folder_when_scout_exists() {
        let s = klip_user(serde_json::json!({}));
        let videos = s.places.videos.clone().unwrap();
        fs::create_dir_all(videos.join("Scout")).unwrap();
        let r = migrate(&s.places);
        assert!(
            matches!(&r.clips, Clips::Kept { folder, .. } if *folder == videos.join("Klip")),
            "{r:?}"
        );
        assert_eq!(
            settings(&s.places).clips.folder.as_deref(),
            Some(videos.join("Klip").to_string_lossy().as_ref())
        );
        assert!(
            videos
                .join("Klip")
                .join("Apex")
                .join("Klip_2026-10-11_10-00-00.mp4")
                .is_file()
        );
    }

    #[cfg(windows)]
    #[test]
    fn keeps_the_old_folder_when_a_clip_is_open() {
        use std::os::windows::fs::OpenOptionsExt;
        let s = klip_user(serde_json::json!({}));
        let videos = s.places.videos.clone().unwrap();
        let clip = videos
            .join("Klip")
            .join("Apex")
            .join("Klip_2026-10-11_10-00-00.mp4");
        // Open without sharing, like a player or the old Klip holding it.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&clip)
            .unwrap();
        let r = migrate(&s.places);
        drop(held);
        assert!(matches!(&r.clips, Clips::Kept { .. }), "{r:?}");
        assert!(clip.is_file());
        assert!(!videos.join("Scout").exists());
        assert_eq!(
            settings(&s.places).clips.folder.as_deref(),
            Some(videos.join("Klip").to_string_lossy().as_ref())
        );
    }

    #[test]
    fn a_chosen_clip_folder_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let mine = tmp.path().join("My clips");
        let s = klip_user(serde_json::json!({ "clips": { "folder": mine } }));
        let r = migrate(&s.places);
        assert_eq!(r.clips, Clips::Untouched);
        assert!(r.settings);
        assert_eq!(
            settings(&s.places).clips.folder.as_deref(),
            Some(mine.to_string_lossy().as_ref())
        );
        assert!(s.places.videos.unwrap().join("Klip").is_dir());
    }

    #[test]
    fn nothing_for_a_new_user_or_after_scout_has_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places {
            config: tmp.path().join("config"),
            videos: Some(tmp.path().join("Videos")),
        };
        assert_eq!(migrate(&places), Report::nothing());
        assert!(!places.config.join("Scout").exists());

        let s = klip_user(serde_json::json!({}));
        let new = s.places.config.join("Scout");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("settings.json"), b"{}").unwrap();
        assert_eq!(migrate(&s.places), Report::nothing());
        assert!(s.places.videos.unwrap().join("Klip").is_dir());
    }
}
