//! Files an export creates next to its output: the packet spool and the `.part` MP4.
//! Each is created with `create_new` (a file someone else left there is never touched,
//! not even deleted) and removed when dropped, unless it was committed to its final name.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::ExportError;

/// A file this export created; removed on drop unless [`OwnedFile::commit`]ted.
#[derive(Debug)]
pub struct OwnedFile {
    path: PathBuf,
    keep: bool,
}

impl OwnedFile {
    /// Create `path` (which must not exist) for writing.
    pub fn create(path: &Path) -> Result<(OwnedFile, File), ExportError> {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(f) => Ok((
                OwnedFile {
                    path: path.to_path_buf(),
                    keep: false,
                },
                f,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ExportError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "{} is in the way (another export, or one that was stopped)",
                        path.display()
                    ),
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rename to `to`, never replacing an existing file (that is
    /// [`klip_mp4::Mp4Error::AlreadyExists`], and this file is removed).
    pub fn commit(mut self, to: &Path) -> Result<(), ExportError> {
        if to.exists() {
            return Err(klip_mp4::Mp4Error::AlreadyExists(to.to_path_buf()).into());
        }
        std::fs::rename(&self.path, to)?;
        self.keep = true;
        Ok(())
    }
}

impl Drop for OwnedFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// `path` with `suffix` appended to its file name (`x.mp4` → `x.mp4.part`).
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stray_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("x.mp4");
        let part = with_suffix(&out, ".part");
        std::fs::write(&part, b"someone else's").unwrap();
        let e = OwnedFile::create(&part).unwrap_err();
        assert!(matches!(e, ExportError::Io(_)), "{e}");
        assert_eq!(std::fs::read(&part).unwrap(), b"someone else's");
        std::fs::remove_file(&part).unwrap();

        let (owned, mut f) = OwnedFile::create(&part).unwrap();
        std::io::Write::write_all(&mut f, b"data").unwrap();
        drop(f);
        owned.commit(&out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"data");
        assert!(!part.exists());

        // Windows can't remove a file that is still open: handles close first.
        let (owned, f) = OwnedFile::create(&part).unwrap();
        drop(f);
        assert!(matches!(
            owned.commit(&out).unwrap_err(),
            ExportError::Mp4(klip_mp4::Mp4Error::AlreadyExists(_))
        ));
        assert!(!part.exists(), "a refused commit removes the part");
        assert_eq!(std::fs::read(&out).unwrap(), b"data");

        let (owned, f) = OwnedFile::create(&part).unwrap();
        drop(f);
        drop(owned);
        assert!(!part.exists(), "dropped = removed");
    }
}
