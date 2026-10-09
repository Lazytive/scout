//! Klip's desktop app: a thin Tauri 2 shell over `klip-engine` (ADR 0014). It runs the
//! engine, forwards its events to the window (`klip://event`) and the tray, and passes
//! the window's commands to it (`klip`). No recording logic lives here.
//!
//! Development and test overrides (environment):
//! - `KLIP_SETTINGS`: settings file instead of the user's.
//! - `KLIP_DISCOVERY`: control-channel discovery file instead of the user's.
//! - `KLIP_WINDOW_POS=x,y`: open the window there at start, without taking focus (tests
//!   put it on a second monitor so whoever uses the first one isn't disturbed).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod ipc;
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
    let b = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::default())
        .title("Klip")
        .inner_size(1180.0, 760.0)
        .min_inner_size(820.0, 520.0);
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

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let settings_path = std::env::var_os("KLIP_SETTINGS")
        .map(PathBuf::from)
        .or_else(klip_engine::default_settings_path)
        .ok_or("no per-user config folder")?;
    let discovery = std::env::var_os("KLIP_DISCOVERY").map(PathBuf::from);
    let window_pos = std::env::var("KLIP_WINDOW_POS")
        .ok()
        .and_then(|s| parse_pos(&s));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .invoke_handler(tauri::generate_handler![ipc::klip])
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
            });

            let tray_state = Arc::new(Mutex::new(tray::TrayState::default()));
            let tray = tray::create(app.handle(), handler.clone(), tray_state.clone(), show_main)?;

            // Engine events → tray and window.
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("klip-events".into())
                .spawn(move || {
                    for e in rx {
                        tray::update(&tray, &tray_state, &e);
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
                && setting(&state.handler, "app", "close_to_tray", true)
            {
                api.prevent_close();
                let _ = window.hide();
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
    // Encoder probes run in a fresh copy of this executable (klip_probe::isolated).
    if let Some(code) = klip_probe::worker_main() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    klip_platform::enable_dpi_awareness();
    if cfg!(target_os = "linux")
        && let Err(e) = klip_platform::register_app_id(klip_platform::APP_ID)
    {
        tracing::warn!(error = %e, "could not register the app id with the portals");
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "Klip could not start");
            ExitCode::FAILURE
        }
    }
}
