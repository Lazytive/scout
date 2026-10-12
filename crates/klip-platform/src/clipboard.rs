//! "Copied files" on the clipboard (M11.4, ADR 0021): paste them in Discord, Explorer,
//! Dolphin… Windows gets CF_HDROP, Linux `text/uri-list` (Wayland data-control when the
//! compositor has it, else X11).
//!
//! On Linux the program that set the clipboard serves it, so one clipboard handle lives
//! for the whole process; Windows keeps the data itself.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::PlatformError;

/// Most files one copy takes.
pub const MAX_COPY_FILES: usize = 1000;

static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();

/// Put these files on the clipboard as copied files. Each must be an absolute path to an
/// existing file.
pub fn copy_files(paths: &[PathBuf]) -> Result<(), PlatformError> {
    if paths.is_empty() {
        return Err(PlatformError::Os("no files to copy".into()));
    }
    if paths.len() > MAX_COPY_FILES {
        return Err(PlatformError::Os(format!(
            "at most {MAX_COPY_FILES} files can be copied at once"
        )));
    }
    if let Some(p) = paths.iter().find(|p| !p.is_absolute() || !p.is_file()) {
        return Err(PlatformError::Os(format!("not a file: {}", p.display())));
    }
    with_clipboard(|c| c.set().file_list(paths))
}

/// The files on the clipboard, when it holds copied files.
pub fn copied_files() -> Result<Vec<PathBuf>, PlatformError> {
    with_clipboard(|c| c.get().file_list())
}

/// The clipboard's text, when it holds text (tests put it back after copying).
pub fn clipboard_text() -> Result<String, PlatformError> {
    with_clipboard(|c| c.get_text())
}

/// Replace the clipboard with text.
pub fn set_clipboard_text(text: &str) -> Result<(), PlatformError> {
    with_clipboard(|c| c.set_text(text))
}

fn with_clipboard<T>(
    f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, PlatformError> {
    let mut slot = CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let clipboard = match slot.as_mut() {
        Some(c) => c,
        None => slot.insert(arboard::Clipboard::new().map_err(clipboard_error)?),
    };
    f(clipboard).map_err(clipboard_error)
}

fn clipboard_error(e: arboard::Error) -> PlatformError {
    match e {
        arboard::Error::ClipboardOccupied => {
            PlatformError::Os("another program is using the clipboard; try again".into())
        }
        arboard::Error::ContentNotAvailable => {
            PlatformError::Os("the clipboard holds something else".into())
        }
        e => PlatformError::Os(format!("clipboard: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_nothing_relative_and_missing_files() {
        assert!(copy_files(&[]).is_err());
        assert!(copy_files(&[PathBuf::from("relative.mp4")]).is_err());
        let missing = std::env::temp_dir().join("klip-no-such-clip-4f1c.mp4");
        assert!(copy_files(&[missing]).is_err());
        let many = vec![std::env::temp_dir(); MAX_COPY_FILES + 1];
        assert!(copy_files(&many).is_err());
    }

    /// Replaces the user's clipboard for a moment (its text is put back), so it runs with
    /// `cargo xtask e2e`, not in `ci`.
    #[test]
    #[ignore = "e2e: uses the desktop clipboard"]
    fn copied_files_read_back() {
        let before = clipboard_text().ok();
        let dir = std::env::temp_dir().join(format!("klip-clip-copy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("Scout clip a.mp4");
        let b = dir.join("b ünïcode.mp4");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        copy_files(&[a.clone(), b.clone()]).unwrap();
        let back = copied_files().unwrap();
        if let Some(text) = before {
            set_clipboard_text(&text).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
        let canon = |p: &PathBuf| p.canonicalize().unwrap_or_else(|_| p.clone());
        let strip = |p: &PathBuf| {
            PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\").to_string())
        };
        assert_eq!(back.len(), 2, "{back:?}");
        assert_eq!(strip(&back[0]), strip(&canon(&a)));
        assert_eq!(strip(&back[1]), strip(&canon(&b)));
    }
}
