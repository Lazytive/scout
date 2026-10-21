//! `cargo xtask bundle`: the NSIS installer wrapped in Scout's setup (ADR 0023). Tauri's
//! installer becomes `Scout_<version>_x64-setup-engine.exe`, `klip-setup` is built with
//! it embedded and takes the installer's name, and with `--sign` that file is signed for
//! the updater (`tauri signer sign`; the engine's own `.sig` is dropped).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{bundle, cargo, npm, release, run_step};

/// Bytes of the files under `dir` (16 levels at most).
pub fn dir_bytes(dir: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![(dir.to_path_buf(), 0u8)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else {
                continue;
            };
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            } else if meta.is_dir() && depth < 16 {
                stack.push((e.path(), depth.saturating_add(1)));
            }
        }
    }
    total
}

/// What a whole install holds on disk: the app and the GStreamer bundle (what the
/// installer unpacks), for the setup's progress bar.
pub fn install_bytes(root: &Path) -> Result<u64> {
    let exe = root.join("target").join("release").join("klip-app.exe");
    let exe_bytes = std::fs::metadata(&exe)
        .with_context(|| format!("sizing {}", exe.display()))?
        .len();
    Ok(exe_bytes.saturating_add(dir_bytes(&root.join(bundle::OUT))))
}

/// How `tauri signer sign` gets the key: it reads `TAURI_SIGNING_PRIVATE_KEY` as the key
/// itself (the CI secret) and `TAURI_SIGNING_PRIVATE_KEY_PATH` as a file, and refuses a
/// path beside the key. `Some(path)` when the key variable holds a file: pass it as the
/// path variable and drop the key variable for the child.
pub fn key_file(key: &str) -> Option<String> {
    Path::new(key).is_file().then(|| key.to_string())
}

/// Wrap the installer in `nsis` (Tauri's `Scout_<version>_x64-setup.exe`) in the setup,
/// which then carries that name; sign it when asked. Returns the setup's path.
pub fn wrap(root: &Path, nsis: &Path, version: &str, sign: bool) -> Result<PathBuf> {
    let name = release::installer_name(version);
    let public = nsis.join(&name);
    let engine = nsis.join(format!("Scout_{version}_x64-setup-engine.exe"));
    if !public.is_file() {
        bail!("no installer at {}", public.display());
    }
    let _ = std::fs::remove_file(&engine);
    std::fs::rename(&public, &engine)
        .with_context(|| format!("renaming the installer to {}", engine.display()))?;
    // The engine's signature (tauri build's) is not the setup's.
    let sig = nsis.join(format!("{name}.sig"));
    let _ = std::fs::remove_file(&sig);

    let bytes = install_bytes(root)?;
    let mut c = cargo();
    c.args(["build", "--release", "-p", "klip-setup"])
        .env("SCOUT_NSIS_PAYLOAD", &engine)
        .env("SCOUT_INSTALL_BYTES", bytes.to_string());
    run_step("setup (klip-setup with the installer inside)", c)?;
    let built = root.join("target").join("release").join("scout-setup.exe");
    std::fs::copy(&built, &public).with_context(|| format!("placing {}", public.display()))?;
    println!("bundle: the setup carries the installer; {bytes} bytes expected on disk");

    if sign {
        let key =
            std::env::var("TAURI_SIGNING_PRIVATE_KEY").context("TAURI_SIGNING_PRIVATE_KEY")?;
        let mut c = npm::npm(root);
        c.args(["run", "tauri", "--", "signer", "sign"])
            .arg(&public);
        if let Some(path) = key_file(&key) {
            c.env_remove("TAURI_SIGNING_PRIVATE_KEY")
                .env("TAURI_SIGNING_PRIVATE_KEY_PATH", path);
        }
        run_step("sign the setup", c)?;
        if !sig.is_file() {
            bail!("tauri signer sign left no {}", sig.display());
        }
    }
    Ok(public)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_file_is_passed_by_path_and_a_key_text_stays_in_its_variable() {
        let dir = std::env::temp_dir().join(format!("scout-setup-sign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("key.txt");
        std::fs::write(&file, "dW50cnVzdGVk").unwrap();
        let path = file.to_string_lossy().into_owned();
        assert_eq!(key_file(&path).as_deref(), Some(path.as_str()));
        assert_eq!(key_file("dW50cnVzdGVk"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_bytes_sums_the_files_below() {
        let dir = std::env::temp_dir().join(format!("scout-setup-bytes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("x.bin"), [1u8; 300]).unwrap();
        std::fs::write(dir.join("a").join("b").join("y.bin"), [1u8; 45]).unwrap();
        assert_eq!(dir_bytes(&dir), 345);
        assert_eq!(dir_bytes(&dir.join("none")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
