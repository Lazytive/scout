//! The settings file: load (never fails), save (atomic).

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::{Issue, MAX_FILE_BYTES, Settings, lenient};

/// How loading went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    /// No file yet: defaults.
    Missing,
    /// Read as it was.
    Loaded,
    /// Some fields were wrong and were repaired (see the issues).
    Repaired,
    /// The file was unusable: defaults.
    Reset,
}

/// Loaded settings and what was wrong with the file.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub settings: Settings,
    pub issues: Vec<Issue>,
    pub status: LoadStatus,
    /// A copy of a repaired or unusable file (`<file>.bad`), kept for the user.
    pub backup: Option<PathBuf>,
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(path.as_os_str());
    s.push(suffix);
    PathBuf::from(s)
}

fn backup(path: &Path, issues: &mut Vec<Issue>) -> Option<PathBuf> {
    let bad = with_suffix(path, ".bad");
    match fs::copy(path, &bad) {
        Ok(_) => Some(bad),
        Err(e) => {
            issues.push(Issue::new(
                "",
                format!("could not keep a copy of the file: {e}"),
            ));
            None
        }
    }
}

/// Load settings from `path`. Never fails: problems give defaults (for the bad parts or
/// the whole file) and are listed in `issues`.
pub fn load(path: &Path) -> Loaded {
    let fail = |message: String| Loaded {
        settings: Settings::default(),
        issues: vec![Issue::new("", message)],
        status: LoadStatus::Reset,
        backup: None,
    };
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                issues: Vec::new(),
                status: LoadStatus::Missing,
                backup: None,
            };
        }
        Err(e) => return fail(format!("could not open the settings file: {e}")),
    };
    let mut bytes = Vec::new();
    if let Err(e) = file
        .take(MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        return fail(format!("could not read the settings file: {e}"));
    }
    let mut issues = Vec::new();
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        issues.push(Issue::new(
            "",
            format!("the file is bigger than {MAX_FILE_BYTES} bytes; using the defaults"),
        ));
        let backup = backup(path, &mut issues);
        return Loaded {
            settings: Settings::default(),
            issues,
            status: LoadStatus::Reset,
            backup,
        };
    }
    let parsed = match std::str::from_utf8(&bytes) {
        Ok(text) => lenient::parse(text.trim_start_matches('\u{feff}')),
        Err(_) => lenient::reset(Issue::new("", "not UTF-8 text; using the defaults")),
    };
    issues.extend(parsed.issues);
    let status = if parsed.reset {
        LoadStatus::Reset
    } else if issues.is_empty() {
        LoadStatus::Loaded
    } else {
        LoadStatus::Repaired
    };
    let backup = match status {
        LoadStatus::Reset | LoadStatus::Repaired => backup(path, &mut issues),
        LoadStatus::Missing | LoadStatus::Loaded => None,
    };
    Loaded {
        settings: parsed.settings,
        issues,
        status,
        backup,
    }
}

/// Write `settings` to `path` atomically (temp file, fsync, rename), creating the folder.
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir)?;
    }
    let tmp = with_suffix(path, ".tmp");
    let res = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(&text)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}
