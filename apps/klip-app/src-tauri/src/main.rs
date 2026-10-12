//! Scout's desktop app: a thin Tauri 2 shell over `klip-engine` (ADR 0014). It runs the
//! engine, forwards its events to the window (`klip://event`) and the tray, and passes
//! the window's commands to it (`klip`). No recording logic lives here.
//!
//! Development and test overrides (environment):
//! - `KLIP_SETTINGS`: settings file instead of the user's.
//! - `KLIP_DISCOVERY`: control-channel discovery file instead of the user's.
//! - `KLIP_START_PAGE=library|editor|settings`: the page the window opens on (tests
//!   take screenshots of an off-screen window they can't click); `editor` needs
//!   `KLIP_START_CLIP=<path of a clip in the folder>`.
//! - `KLIP_WINDOW_POS=x,y`: open the window there at start, without taking focus (tests
//!   put it on a second monitor so whoever uses the first one isn't disturbed).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod cue;
mod ipc;
mod logging;
mod overlay;
mod thumb_scheme;
mod tray;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use klip_control::{Handler, commands};
use klip_engine::{Engine, EngineOptions};
use serde_json::Value;
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

const MAIN: &str = "main";

pub(crate) struct AppState {
    pub(crate) handler: Arc<dyn Handler>,
    engine: Mutex<Option<Engine>>,
    /// The files the editor's preview may read through the asset protocol, oldest
    /// first (`ipc::editor_media` allows what the engine names and forbids the oldest
    /// beyond `ipc::MAX_MEDIA_FILES`).
    pub(crate) media: Mutex<std::collections::VecDeque<std::path::PathBuf>>,
}

fn setting(handler: &Arc<dyn Handler>, section: &str, key: &str, default: bool) -> bool {
    handler
        .handle(commands::SETTINGS_GET, &Value::Null)
        .ok()
        .and_then(|v| v.get("settings")?.get(section)?.get(key)?.as_bool())
        .unwrap_or(default)
}

fn parse_pos(s: &str) -> Option<(i32, i32)> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Create the main window: hidden (tray start), shown and focused, or, for tests, shown
/// at `pos` without being activated (created visible: showing a hidden window later
/// would take the focus from whatever the user is doing).
fn create_main<R: Runtime>(
    app: &AppHandle<R>,
    visible: bool,
    pos: Option<(i32, i32)>,
) -> tauri::Result<()> {
    let mut b = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::default())
        .title("Scout")
        .inner_size(1180.0, 760.0)
        .min_inner_size(820.0, 520.0);
    // Windows: Scout draws its own title bar (M11.3; the window keeps its shadow, rounded
    // corners and resize edges). Linux keeps the desktop's (KDE users expect it, and an
    // undecorated window there can't be resized from its edges).
    #[cfg(windows)]
    {
        b = b
            .decorations(false)
            .shadow(true)
            .initialization_script("window.__KLIP_CHROME__ = true;");
    }
    if let Ok(page) = std::env::var("KLIP_START_PAGE")
        && matches!(page.as_str(), "library" | "editor" | "settings")
    {
        b = b.initialization_script(format!("window.__KLIP_START_PAGE__ = '{page}';"));
    }
    if let Ok(clip) = std::env::var("KLIP_START_CLIP")
        && let Ok(js) = serde_json::to_string(&clip)
    {
        b = b.initialization_script(format!("window.__KLIP_START_CLIP__ = {js};"));
    }
    if let Some((x, y)) = pos {
        b.position(f64::from(x), f64::from(y))
            .visible(true)
            .focused(false)
            .build()?;
        return Ok(());
    }
    let w = b.visible(false).build()?;
    w.center()?;
    if visible {
        w.show()?;
        w.set_focus()?;
    }
    Ok(())
}

/// Start with the OS when the user switches `app.autostart` (only on a change, so a
/// choice made in the OS's own startup-apps list is not overridden at every start).
fn sync_autostart<R: Runtime>(app: &AppHandle<R>, settings: &Value, last: &mut Option<bool>) {
    use tauri_plugin_autostart::ManagerExt;
    let Some(want) = settings
        .get("app")
        .and_then(|a| a.get("autostart"))
        .and_then(Value::as_bool)
    else {
        return;
    };
    let launcher = app.autolaunch();
    let first = last.is_none();
    let changed = last.replace(want) != Some(want);
    // The first event only records the value unless it differs from the OS.
    if first && launcher.is_enabled().ok() == Some(want) {
        return;
    }
    if !changed && !first {
        return;
    }
    let res = if want {
        launcher.enable()
    } else {
        launcher.disable()
    };
    if let Err(e) = res {
        tracing::warn!(error = %e, "start with the OS");
    }
}

/// Bring the main window up (tray click, second launch).
fn show_main<R: Runtime>(app: &AppHandle<R>) {
    let window = match app.get_webview_window(MAIN) {
        Some(w) => Some(w),
        None => create_main(app, false, None)
            .ok()
            .and_then(|()| app.get_webview_window(MAIN)),
    };
    if let Some(w) = window {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Klip is now Scout (ADR 0020): move the user's settings, themes and clip folder over
/// once, before the engine reads them. Not for a custom settings file (tests).
fn migrate_from_klip() {
    if std::env::var_os("KLIP_SETTINGS").is_some() {
        return;
    }
    let Some(places) = klip_engine::migrate::Places::for_user() else {
        return;
    };
    let r = klip_engine::migrate::migrate(&places);
    if r.settings {
        tracing::info!(clips = ?r.clips, themes = r.theme_files, "moved over from Klip");
    }
    for e in &r.errors {
        tracing::warn!(error = %e, "moving over from Klip");
    }
}

/// Klip's "start with the OS" entry (ADR 0020): when it is on, it is removed and Scout's
/// is switched on instead (the OS would otherwise keep starting it under the old name,
/// and switching Scout's off would not stop it).
fn migrate_autostart<R: Runtime>(app: &AppHandle<R>) {
    use tauri_plugin_autostart::ManagerExt;
    let Some(exe) = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
    else {
        return;
    };
    let Ok(old) = auto_launch::AutoLaunchBuilder::new()
        .set_app_name(klip_engine::migrate::LEGACY_NAME)
        .set_app_path(&exe)
        .build()
    else {
        return;
    };
    if old.is_enabled().unwrap_or(false)
        && let Err(e) = old
            .disable()
            .map_err(|e| e.to_string())
            .and_then(|()| app.autolaunch().enable().map_err(|e| e.to_string()))
    {
        tracing::warn!(error = %e, "moving start with the OS over from Klip");
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    migrate_from_klip();
    let settings_path = std::env::var_os("KLIP_SETTINGS")
        .map(PathBuf::from)
        .or_else(klip_engine::default_settings_path)
        .ok_or("no per-user config folder")?;
    let discovery = std::env::var_os("KLIP_DISCOVERY").map(PathBuf::from);
    let window_pos = std::env::var("KLIP_WINDOW_POS")
        .ok()
        .and_then(|s| parse_pos(&s));

    // One Scout per user: a second start shows the running one. A Scout on its own
    // settings file (KLIP_SETTINGS: tests, development) runs beside it instead, so a
    // test never pops the user's window.
    let mut builder = tauri::Builder::default();
    if std::env::var_os("KLIP_SETTINGS").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }));
    }
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_drag::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            ipc::klip,
            ipc::overlay_close,
            ipc::editor_media,
            ipc::debug_info,
            ipc::debug_save_report,
            ipc::open_logs
        ])
        // Library thumbnails for <img> tags (thumb_scheme.rs).
        .register_asynchronous_uri_scheme_protocol(
            thumb_scheme::SCHEME,
            |ctx, request, responder| {
                let path = request.uri().path().to_string();
                let handler = ctx
                    .app_handle()
                    .try_state::<AppState>()
                    .map(|s| s.handler.clone());
                tauri::async_runtime::spawn_blocking(move || {
                    let response = match handler {
                        Some(h) => thumb_scheme::respond(&h, &path),
                        None => {
                            let mut r = tauri::http::Response::new(Vec::new());
                            *r.status_mut() = tauri::http::StatusCode::SERVICE_UNAVAILABLE;
                            r
                        }
                    };
                    responder.respond(response);
                });
            },
        )
        .setup(move |app| {
            let (tx, rx) = std::sync::mpsc::sync_channel(klip_engine::events::QUEUE);
            let mut opts = EngineOptions::new(settings_path.clone());
            opts.discovery = discovery.clone();
            opts.events = Some(tx);
            let engine = Engine::start(opts)?;
            let handler = engine.handler();
            app.manage(AppState {
                handler: handler.clone(),
                engine: Mutex::new(Some(engine)),
                media: Mutex::new(std::collections::VecDeque::new()),
            });
            if std::env::var_os("KLIP_SETTINGS").is_none() {
                migrate_autostart(app.handle());
            }
            // Custom theme pictures (ADR 0019) are shown through the asset protocol:
            // the themes folder only, not recursively.
            let themes = klip_engine::themes_dir(&settings_path);
            if let Err(e) = std::fs::create_dir_all(&themes)
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    app.asset_protocol_scope()
                        .allow_directory(&themes, false)
                        .map_err(|e| e.to_string())
                })
            {
                tracing::warn!(error = %e, "theme pictures");
            }

            let prefs = handler
                .handle(commands::SETTINGS_GET, &Value::Null)
                .ok()
                .and_then(|v| v.get("settings").cloned())
                .map(|s| overlay::Prefs::from_settings(&s))
                .unwrap_or_else(|| overlay::Prefs::from_settings(&Value::Null));
            let overlay = overlay::Overlay::create(app.handle(), prefs)?;
            app.manage(overlay::OverlayHandle(overlay.clone()));

            let tray_state = Arc::new(Mutex::new(tray::TrayState::default()));
            let tray = tray::create(app.handle(), handler.clone(), tray_state.clone(), show_main)?;

            // Engine events → tray and window.
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("klip-events".into())
                .spawn(move || {
                    let mut autostart: Option<bool> = None;
                    for e in rx {
                        tray::update(&tray, &tray_state, &e);
                        overlay.on_event(&e);
                        if let klip_engine::Event::SettingsChanged { settings } = &e {
                            sync_autostart(&handle, settings, &mut autostart);
                        }
                        let _ = handle.emit("klip://event", e.to_json());
                    }
                })?;

            // `app.quit` / `recorder.stop` from the control channel ends the app.
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("klip-exit-watch".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_millis(250));
                        let quit = handle.try_state::<AppState>().is_some_and(|s| {
                            s.engine
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .as_ref()
                                .is_some_and(Engine::exit_requested)
                        });
                        if quit {
                            handle.exit(0);
                            return;
                        }
                    }
                })?;

            let start_minimized = setting(&handler, "app", "start_minimized", true);
            create_main(
                app.handle(),
                window_pos.is_some() || !start_minimized,
                window_pos,
            )?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == MAIN
                && let Some(state) = window.app_handle().try_state::<AppState>()
            {
                if setting(&state.handler, "app", "close_to_tray", true) {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    // The hidden overlay window would keep the app alive otherwise.
                    window.app_handle().exit(0);
                }
            }
        })
        .build(tauri::generate_context!())?;

    app.run(|handle, event| {
        if let RunEvent::Exit = event
            && let Some(state) = handle.try_state::<AppState>()
        {
            let engine = state
                .engine
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            // Stops the recorder; a running recording is finalised first.
            if let Some(e) = engine {
                e.shutdown();
            }
        }
    });
    Ok(())
}

fn main() -> ExitCode {
    // An installed Scout carries its own GStreamer (cargo xtask bundle); before any thread.
    let bundled_gstreamer = klip_platform::use_bundled_gstreamer();
    let kind = logging::kind();
    klip_platform::set_env_before_threads(&logging::gstreamer_env(kind));
    // Every copy of the exe logs to a file (logging.rs); workers to the file only.
    logging::init(kind);
    if kind == "app" {
        logging::prune();
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            full_logs = logging::FULL,
            bundled_gstreamer,
            logs = %logging::dir().map(|d| d.display().to_string()).unwrap_or_default(),
            "Scout starting"
        );
    }
    // Encoder probes run in a fresh copy of this executable (klip_probe::isolated).
    if let Some(code) = klip_probe::worker_main() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    // Library thumbnails too (klip_engine::media_worker): hostile files can't crash us.
    if let Some(code) = klip_engine::media_worker::worker_main() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    // The engine's recorder runs in a copy of this executable too (ADR 0016).
    if let Some(code) = klip_engine::worker_main() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    klip_platform::enable_dpi_awareness();
    if cfg!(target_os = "linux")
        && let Err(e) = klip_platform::register_app_id(klip_platform::APP_ID)
    {
        tracing::warn!(error = %e, "could not register the app id with the portals");
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "Scout could not start");
            ExitCode::FAILURE
        }
    }
}
