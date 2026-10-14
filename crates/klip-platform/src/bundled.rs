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

/// Linux in a container (distrobox, toolbox): the host's D-Bus system bus, for
/// `DBUS_SYSTEM_BUS_ADDRESS`, when the container has none of its own. PipeWire asks RTKit
/// there for its real-time audio thread; without it Scout's audio capture ran at normal
/// priority and lost audio on a busy machine (found on Bazzite, 2026-10-14: half of a
/// tone with one CPU core busy; none with the host's bus). `None`: a value the user set,
/// the container's own bus, or no host bus.
pub fn container_system_bus() -> Option<std::ffi::OsString> {
    container_system_bus_in(
        std::path::Path::new("/"),
        std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS").is_some(),
    )
}

fn container_system_bus_in(root: &std::path::Path, set: bool) -> Option<std::ffi::OsString> {
    if set || root.join("run/dbus/system_bus_socket").exists() {
        return None;
    }
    let host = root.join("run/host/run/dbus/system_bus_socket");
    let path = host.to_str()?;
    // An address needs no escaping for a plain path.
    let plain = path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b));
    (plain && host.exists()).then(|| format!("unix:path={path}").into())
}

/// Set environment variables at start-up (a tester build's GStreamer log). Call first
/// thing in `main`, before any thread starts (child processes inherit them).
#[allow(unsafe_code)] // Setting environment variables; the block says why it is sound.
pub fn set_env_before_threads(vars: &[(&str, std::ffi::OsString)]) {
    for (key, value) in vars {
        // SAFETY: called at the start of `main`, before this process starts any thread,
        // so nothing reads or writes the environment at the same time.
        unsafe { std::env::set_var(key, value) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_system_bus_only_when_the_host_has_one_and_we_have_none() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        assert_eq!(container_system_bus_in(r, false), None, "no bus anywhere");
        std::fs::create_dir_all(r.join("run/host/run/dbus")).unwrap();
        std::fs::write(r.join("run/host/run/dbus/system_bus_socket"), b"").unwrap();
        let want = format!(
            "unix:path={}",
            r.join("run/host/run/dbus/system_bus_socket").display()
        );
        let plain = r
            .to_str()
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b));
        if plain {
            assert_eq!(
                container_system_bus_in(r, false),
                Some(want.into()),
                "the host's"
            );
        }
        assert_eq!(
            container_system_bus_in(r, true),
            None,
            "the user's value wins"
        );
        std::fs::create_dir_all(r.join("run/dbus")).unwrap();
        std::fs::write(r.join("run/dbus/system_bus_socket"), b"").unwrap();
        assert_eq!(container_system_bus_in(r, false), None, "our own bus");
    }
}
