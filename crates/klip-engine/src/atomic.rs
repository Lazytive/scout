//! Atomic file writes: a crash or power loss leaves the old file or the new one, never a
//! short one under the real name.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Write `data` to `path` via `<path>.tmp`: write, fsync, rename. The temp file is
/// removed when any step fails.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let r = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        // Without it the rename can reach the disk before the bytes do, and a crash
        // leaves an empty file that looks valid (a 0-byte poster cached for good).
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/poster.jpg");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two!").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two!");
        let names: Vec<_> = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn a_failed_write_keeps_the_old_file_and_removes_the_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // The target is a folder: the rename fails after the temp file was written.
        let p = dir.path().join("taken");
        fs::create_dir_all(p.join("inside")).unwrap();
        assert!(write_atomic(&p, b"x").is_err());
        assert!(p.join("inside").is_dir());
        assert!(!dir.path().join("taken.tmp").exists());
    }
}
