//! The project file: load (never fails), save (atomic).

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::project::{Issue, MAX_FILE_BYTES, Project};

/// How loading went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    /// No file yet: a new project.
    Missing,
    /// Read as it was.
    Loaded,
    /// Some fields were wrong and were repaired (see the issues).
    Repaired,
    /// The file was unusable: a new project.
    Reset,
}

/// A loaded project and what was wrong with the file.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub project: Project,
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

/// Load the project at `path`. Never fails: problems give a new project (for the bad
/// parts or the whole file) and are listed in `issues`.
pub fn load(path: &Path) -> Loaded {
    let reset = |issues: Vec<Issue>, backup: Option<PathBuf>| Loaded {
        project: Project::default(),
        issues,
        status: LoadStatus::Reset,
        backup,
    };
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                project: Project::default(),
                issues: Vec::new(),
                status: LoadStatus::Missing,
                backup: None,
            };
        }
        Err(e) => {
            return reset(
                vec![Issue::new(
                    "",
                    format!("could not open the project file: {e}"),
                )],
                None,
            );
        }
    };
    let mut bytes = Vec::new();
    if let Err(e) = file
        .take(MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        return reset(
            vec![Issue::new(
                "",
                format!("could not read the project file: {e}"),
            )],
            None,
        );
    }
    let mut issues = Vec::new();
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        issues.push(Issue::new(
            "",
            format!("the file is bigger than {MAX_FILE_BYTES} bytes; using a new project"),
        ));
        let backup = backup(path, &mut issues);
        return reset(issues, backup);
    }
    let value = match std::str::from_utf8(&bytes) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}')),
        Err(e) => {
            issues.push(Issue::new("", format!("not UTF-8 text: {e}")));
            let backup = backup(path, &mut issues);
            return reset(issues, backup);
        }
    };
    let (project, status) = match value {
        Ok(v @ serde_json::Value::Object(_)) => {
            let (p, more) = Project::from_value(v);
            issues.extend(more);
            let status = if issues.is_empty() {
                LoadStatus::Loaded
            } else {
                LoadStatus::Repaired
            };
            (p, status)
        }
        Ok(_) => {
            issues.push(Issue::new("", "not a JSON object; using a new project"));
            (Project::default(), LoadStatus::Reset)
        }
        Err(e) => {
            issues.push(Issue::new(
                "",
                format!("not valid JSON ({e}); using a new project"),
            ));
            (Project::default(), LoadStatus::Reset)
        }
    };
    let backup = match status {
        LoadStatus::Reset | LoadStatus::Repaired => backup(path, &mut issues),
        LoadStatus::Missing | LoadStatus::Loaded => None,
    };
    Loaded {
        project,
        issues,
        status,
        backup,
    }
}

/// Write `project` to `path` atomically (temp file, fsync, rename), creating the folder.
pub fn save(path: &Path, project: &Project) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(project).map_err(std::io::Error::other)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Piece, Speed};

    #[test]
    fn missing_file_is_a_new_project() {
        let dir = tempfile::tempdir().unwrap();
        let l = load(&dir.path().join("none.json"));
        assert_eq!(l.status, LoadStatus::Missing);
        assert_eq!(l.project, Project::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("clip.json");
        let mut p = Project::new(8000);
        p.pieces.push(Piece {
            start_ms: 1000,
            end_ms: 3000,
            speed: Speed::Double,
        });
        save(&path, &p).unwrap();
        let l = load(&path);
        assert_eq!(l.status, LoadStatus::Loaded);
        assert_eq!(l.project, p);
        assert!(!dir.path().join("deep").join("clip.json.tmp").exists());
    }

    #[test]
    fn garbage_resets_with_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.json");
        fs::write(&path, b"{not json").unwrap();
        let l = load(&path);
        assert_eq!(l.status, LoadStatus::Reset);
        assert_eq!(
            l.backup.as_deref(),
            Some(dir.path().join("clip.json.bad").as_path())
        );
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(load(&path).status, LoadStatus::Reset);
        fs::write(&path, b"[1,2]").unwrap();
        assert_eq!(load(&path).status, LoadStatus::Reset);
        fs::write(&path, vec![b' '; (MAX_FILE_BYTES as usize) + 1]).unwrap();
        assert_eq!(load(&path).status, LoadStatus::Reset);
    }

    #[test]
    fn repaired_file_is_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.json");
        fs::write(
            &path,
            br#"{"source_ms": 1000, "pieces": [{"start_ms": 5, "end_ms": 5}]}"#,
        )
        .unwrap();
        let l = load(&path);
        assert_eq!(l.status, LoadStatus::Repaired);
        assert!(l.backup.is_some());
        assert!(l.project.pieces.is_empty());
    }
}
