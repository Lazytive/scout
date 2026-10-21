//! Scout's setup (ADR 0023): the installer in the app's own look. A Tauri window from the
//! app's own `setup.html` wraps the NSIS installer it carries: the window shows the
//! install, runs the installer silently, and opens Scout at the end. `/S` installs with
//! no window at all; `/P` (what the updater passes) needs no click; without WebView2 the
//! wizard inside runs instead. Nothing here touches the engine.
//!
//! `tauri::command` generates a wrapper whose argument plumbing contains `unreachable!()`;
//! our code never reaches it, so that lint is allowed in this crate.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::unreachable)]

mod install;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tauri::{AppHandle, Emitter as _, WebviewUrl, WebviewWindowBuilder};

use install::{Args, dir_bytes, inner_args, install_dir, parse_args, progress, work_dir};

/// The NSIS installer, put here by `cargo xtask bundle` (empty in a development build).
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.exe"));
/// Scout's executable as the installer places it: the `klip-app` crate's binary name,
/// which Tauri keeps (the Start menu shortcut and the Programs entry say "Scout").
const APP_EXE: &str = "klip-app.exe";

/// What a whole install holds on disk, for the progress bar (measured at bundle time).
fn expected_bytes() -> u64 {
    option_env!("SCOUT_INSTALL_BYTES")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(60_000_000)
}

struct SetupState {
    args: Args,
    dir: PathBuf,
    running: Mutex<bool>,
}

/// Write the installer to its work folder.
fn extract() -> Result<PathBuf, String> {
    if PAYLOAD.is_empty() {
        return Err("this build of the setup carries no installer (a development build)".into());
    }
    let dir = work_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let exe = dir.join("Scout-installer.exe");
    std::fs::write(&exe, PAYLOAD).map_err(|e| format!("could not write the installer: {e}"))?;
    Ok(exe)
}

fn cleanup(exe: &Path) {
    let _ = std::fs::remove_file(exe);
    if let Some(dir) = exe.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Run the installer and wait: silently into `dir`, or as the wizard (no WebView2) with
/// the update and run flags as the updater gave them. Its exit code says how it went.
fn run_inner(exe: &Path, args: &Args, dir: &Path, wizard: bool) -> Result<(), String> {
    let mut c = Command::new(exe);
    if wizard {
        if args.update {
            c.arg("/UPDATE");
        }
        if args.run_after {
            c.arg("/R");
        }
    } else {
        #[cfg(windows)]
        {
            // `/D=` must reach NSIS unquoted, spaces and all.
            use std::os::windows::process::CommandExt as _;
            for a in inner_args(args, dir) {
                c.raw_arg(a);
            }
        }
        #[cfg(not(windows))]
        c.args(inner_args(args, dir));
    }
    let status = c
        .status()
        .map_err(|e| format!("could not start the installer: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the installer ended with {status}"))
    }
}

fn open_scout(dir: &Path, app_args: &[String]) -> Result<(), String> {
    Command::new(dir.join(APP_EXE))
        .args(app_args)
        .current_dir(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start Scout: {e}"))
}

/// Install on a thread: `setup://progress` every 150 ms, then `setup://done`.
fn install_thread(app: AppHandle, state: Arc<SetupState>) {
    std::thread::spawn(move || {
        let dir = state.dir.clone();
        let args = state.args.clone();
        let result = extract().and_then(|exe| {
            let done = Arc::new(Mutex::new(false));
            let watcher = {
                let (app, dir, done) = (app.clone(), dir.clone(), done.clone());
                std::thread::spawn(move || {
                    let expected = expected_bytes();
                    while !*done.lock().unwrap_or_else(PoisonError::into_inner) {
                        let bytes = dir_bytes(&dir);
                        let _ = app.emit(
                            "setup://progress",
                            json!({ "pct": progress(bytes, expected), "bytes": bytes }),
                        );
                        std::thread::sleep(Duration::from_millis(150));
                    }
                })
            };
            let r = run_inner(&exe, &args, &dir, false);
            *done.lock().unwrap_or_else(PoisonError::into_inner) = true;
            let _ = watcher.join();
            cleanup(&exe);
            r
        });
        *state.running.lock().unwrap_or_else(PoisonError::into_inner) = false;
        let _ = app.emit(
            "setup://done",
            match &result {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "ok": false, "error": e }),
            },
        );
    });
}

#[tauri::command]
fn setup_info(state: tauri::State<'_, Arc<SetupState>>) -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "dir": state.dir.to_string_lossy(),
        "passive": state.args.passive,
        "run_after": state.args.run_after,
        "update": state.args.update,
        "has_installer": !PAYLOAD.is_empty(),
        "expected_bytes": expected_bytes(),
    })
}

#[tauri::command]
fn setup_install(app: AppHandle, state: tauri::State<'_, Arc<SetupState>>) -> Result<(), String> {
    {
        let mut running = state.running.lock().unwrap_or_else(PoisonError::into_inner);
        if *running {
            return Err("already installing".into());
        }
        *running = true;
    }
    install_thread(app, state.inner().clone());
    Ok(())
}

#[tauri::command]
fn setup_open(app: AppHandle, state: tauri::State<'_, Arc<SetupState>>) -> Result<(), String> {
    open_scout(&state.dir, &state.args.app_args)?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn setup_quit(app: AppHandle) {
    app.exit(0);
}

fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1));
    let dir = install_dir(&args).ok_or_else(|| "no per-user folder (LOCALAPPDATA)".to_string())?;
    if args.silent {
        let exe = extract()?;
        let r = run_inner(&exe, &args, &dir, false);
        cleanup(&exe);
        r?;
        if args.run_after {
            open_scout(&dir, &args.app_args)?;
        }
        return Ok(());
    }
    // Without WebView2 the window can't draw: the wizard inside takes over (it installs
    // WebView2 as Tauri's installer does).
    if tauri::webview_version().is_err() {
        let exe = extract()?;
        let r = run_inner(&exe, &args, &dir, true);
        cleanup(&exe);
        return r;
    }
    let state = Arc::new(SetupState {
        args,
        dir,
        running: Mutex::new(false),
    });
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            setup_info,
            setup_install,
            setup_open,
            setup_quit
        ])
        .setup(|app| {
            let mut b =
                WebviewWindowBuilder::new(app, "setup", WebviewUrl::App("setup.html".into()))
                    .title("Scout Setup")
                    .inner_size(560.0, 460.0)
                    .resizable(false)
                    .maximizable(false);
            // Windows: Scout's own title bar, as the app draws it.
            #[cfg(windows)]
            {
                b = b
                    .decorations(false)
                    .shadow(true)
                    .initialization_script("window.__KLIP_CHROME__ = true;");
            }
            let w = b.visible(false).build()?;
            w.center()?;
            w.show()?;
            w.set_focus()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("scout-setup: {e}");
            ExitCode::FAILURE
        }
    }
}
