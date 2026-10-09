//! Hand a file to the desktop: open it in the user's default app, or show it in the file
//! manager (the library's "Open" and "Show in folder"). Each call runs on a short-lived
//! thread of its own (Windows: a single-threaded COM apartment, as the shell wants).

use std::path::{Path, PathBuf};

use crate::PlatformError;

fn on_thread(path: &Path, f: fn(&Path) -> Result<(), PlatformError>) -> Result<(), PlatformError> {
    let owned: PathBuf = path.to_path_buf();
    std::thread::Builder::new()
        .name("klip-shell".into())
        .spawn(move || f(&owned))
        .map_err(|e| PlatformError::Os(format!("no thread for the shell: {e}")))?
        .join()
        .unwrap_or_else(|_| Err(PlatformError::Os("the shell thread panicked".into())))
}

/// Open a file in the program the user picked for its type.
pub fn open_file(path: &Path) -> Result<(), PlatformError> {
    on_thread(path, imp::open)
}

/// Show a file selected in the file manager (Linux without a file manager that supports
/// selecting: its folder is opened).
pub fn reveal_file(path: &Path) -> Result<(), PlatformError> {
    on_thread(path, imp::reveal)
}

#[cfg(windows)]
#[allow(unsafe_code)] // Shell calls; each block says why it is sound.
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize, IBindCtx,
    };
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};

    use crate::PlatformError;

    struct Sta(bool);

    impl Drop for Sta {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: pairs with the successful CoInitializeEx in `sta` on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn sta() -> Sta {
        // SAFETY: CoInitializeEx may be called on any thread; this one is ours alone
        // (`on_thread`), so no other apartment model is in use on it.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Sta(hr.is_ok())
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn open(path: &Path) -> Result<(), PlatformError> {
        let _com = sta();
        let file = wide(path);
        // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
        let r = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(file.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 mean success (the HINSTANCE is a status code here).
        let code = r.0 as usize;
        if code > 32 {
            Ok(())
        } else {
            Err(PlatformError::Os(format!(
                "Windows could not open the file (code {code})"
            )))
        }
    }

    pub(super) fn reveal(path: &Path) -> Result<(), PlatformError> {
        let _com = sta();
        let file = wide(path);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: `file` is NUL-terminated and outlives the call; `pidl` receives an item
        // list allocated by the shell, freed below with ILFree.
        unsafe { SHParseDisplayName(PCWSTR(file.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }
            .map_err(|e| PlatformError::Os(format!("finding the file: {e}")))?;
        // SAFETY: `pidl` is the valid absolute item list from above (it names the file,
        // which the folder view selects).
        let r = unsafe { SHOpenFolderAndSelectItems(pidl, None, 0) };
        // SAFETY: frees the list SHParseDisplayName allocated; not used afterwards.
        unsafe { ILFree(Some(pidl)) };
        r.map_err(|e| PlatformError::Os(format!("opening the folder: {e}")))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    use std::process::{Command, Stdio};

    use crate::PlatformError;

    fn spawn(cmd: &mut Command) -> Result<bool, PlatformError> {
        let status = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| PlatformError::Os(e.to_string()))?;
        Ok(status.success())
    }

    pub(super) fn open(path: &Path) -> Result<(), PlatformError> {
        if spawn(Command::new("xdg-open").arg(path))? {
            Ok(())
        } else {
            Err(PlatformError::Os("xdg-open could not open the file".into()))
        }
    }

    /// `file://` URI of an absolute path (bytes outside the unreserved set escaped).
    fn file_uri(path: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut out = String::from("file://");
        for b in path.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(*b));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    pub(super) fn reveal(path: &Path) -> Result<(), PlatformError> {
        // The freedesktop file-manager interface selects the file (Dolphin, Nautilus,
        // Nemo, Thunar…); without one, open the folder.
        let shown = spawn(Command::new("gdbus").args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.FileManager1",
            "--object-path",
            "/org/freedesktop/FileManager1",
            "--method",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("['{}']", file_uri(path)),
            "",
        ]))
        .unwrap_or(false);
        if shown {
            return Ok(());
        }
        let dir = path.parent().unwrap_or(path);
        if spawn(Command::new("xdg-open").arg(dir))? {
            Ok(())
        } else {
            Err(PlatformError::Os("could not open the folder".into()))
        }
    }
}
