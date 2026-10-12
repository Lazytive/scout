//! Scout's own copy of GStreamer (an installed Scout, `cargo xtask bundle`): the DLLs sit
//! beside the exe (Windows loads them from there first) and the plugins in
//! `gstreamer-1.0\`. GStreamer is pointed at that folder, its scanner and a registry of
//! Scout's own, and away from any other plugin folders the user's environment names, so
//! a system GStreamer (or its GPL plugins) is never mixed in.

use std::path::{Path, PathBuf};

/// The plugin folder beside the exe that marks a bundled Scout.
pub const BUNDLED_PLUGIN_DIR: &str = "gstreamer-1.0";

/// Point GStreamer at the bundled copy when there is one; whether there was. Call first
/// thing in `main`, before any thread starts (child processes inherit the result).
#[allow(unsafe_code)] // Setting environment variables; the block says why it is sound.
pub fn use_bundled_gstreamer() -> bool {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    else {
        return false;
    };
    let plugins = dir.join(BUNDLED_PLUGIN_DIR);
    if !cfg!(windows) || !plugins.is_dir() {
        return false;
    }
    let registry: Option<PathBuf> = std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Scout"))
        .filter(|d| std::fs::create_dir_all(d).is_ok())
        .map(|d| d.join("gstreamer-1.0-registry.bin"));
    // SAFETY: called at the start of `main`, before this process starts any thread, so
    // nothing reads or writes the environment at the same time (`set_var`'s condition).
    unsafe {
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
        std::env::set_var("GST_PLUGIN_SCANNER_1_0", dir.join("gst-plugin-scanner.exe"));
        if let Some(r) = registry {
            std::env::set_var("GST_REGISTRY_1_0", r);
        }
        for other in [
            "GST_PLUGIN_PATH_1_0",
            "GST_PLUGIN_PATH",
            "GST_PLUGIN_SYSTEM_PATH",
        ] {
            std::env::remove_var(other);
        }
    }
    true
}
