//! The overlay (M6.9): a separate transparent window, always on top, click-through,
//! never activated (it gets `WS_EX_NOACTIVATE` and is shown with `SWP_NOACTIVATE`, so the
//! game keeps the keyboard focus), placed in the chosen corner of the game's monitor.
//! - Minimal: a "Clip saved" card for `overlay.seconds`, the thumbnail fades in when the
//!   engine has made it.
//! - Full: the card, a small dot while a recording runs, and the quick menu on its
//!   hotkey (the menu is the only state that takes clicks and focus; closing it hands the
//!   focus back to the game).
//! - Off, or a game in exclusive fullscreen (a window can't show over it): only the
//!   sound and/or system notification, if switched on. Those play in every mode.
//!
//! Test override (environment): `KLIP_OVERLAY_POS=x,y` puts the overlay there instead
//! of on the game's monitor; `KLIP_NO_FOCUS=1` keeps the quick menu from taking focus.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use klip_engine::Event;
use serde_json::{Value, json};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

pub const LABEL: &str = "overlay";
/// Window sizes in logical pixels (the card has a 6 px margin for its shadow).
const CARD: (f64, f64) = (392.0, 100.0);
const DOT: (f64, f64) = (26.0, 26.0);
const MENU: (f64, f64) = (420.0, 330.0);
/// Distance from the monitor's edge, logical pixels.
const MARGIN: f64 = 18.0;
/// The quick menu closes by itself after this long.
const MENU_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RECENT: usize = 5;

/// The overlay settings the window needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub mode: String,
    pub corner: String,
    pub seconds: u64,
    pub sound: bool,
    pub notification: bool,
    pub dot: bool,
}

impl Prefs {
    /// From the engine's settings JSON (missing values: the defaults).
    pub fn from_settings(settings: &Value) -> Prefs {
        let o = settings.get("overlay").unwrap_or(&Value::Null);
        let s = |k: &str, d: &str| o.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let b = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        Prefs {
            mode: s("mode", "minimal"),
            corner: s("corner", "top-right"),
            seconds: o
                .get("seconds")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .clamp(1, 10),
            sound: b("sound", true),
            notification: b("notification", false),
            dot: b("recording_dot", true),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Showing {
    Nothing,
    Card,
    Dot,
    Menu,
}

struct State {
    prefs: Prefs,
    showing: Showing,
    recording: bool,
    /// The followed game's window: its monitor gets the overlay, and it gets the focus
    /// back after the quick menu.
    game_window: Option<u64>,
    recent: Vec<(String, u64)>,
}

pub struct Overlay {
    app: AppHandle,
    window: WebviewWindow,
    hwnd: Option<u64>,
    state: Mutex<State>,
    /// Bumped by every show; a hide timer only acts if nothing was shown since.
    generation: AtomicU64,
    next_card: AtomicU64,
    pos_override: Option<(i32, i32)>,
    no_focus: bool,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Top-left of a `w`x`h` box in `corner` of the monitor at (`mx`, `my`) sized `mw`x`mh`.
pub fn corner_position(
    corner: &str,
    (mx, my, mw, mh): (i32, i32, u32, u32),
    (w, h): (u32, u32),
    margin: u32,
) -> (i32, i32) {
    let (mx64, my64, m) = (i64::from(mx), i64::from(my), i64::from(margin));
    let right = mx64
        .saturating_add(i64::from(mw))
        .saturating_sub(i64::from(w))
        .saturating_sub(m);
    let bottom = my64
        .saturating_add(i64::from(mh))
        .saturating_sub(i64::from(h))
        .saturating_sub(m);
    let left = mx64.saturating_add(m);
    let top = my64.saturating_add(m);
    let (x, y) = match corner {
        "top-left" => (left, top),
        "bottom-left" => (left, bottom),
        "bottom-right" => (right, bottom),
        _ => (right, top),
    };
    (
        i32::try_from(x).unwrap_or(mx),
        i32::try_from(y).unwrap_or(my),
    )
}

impl Overlay {
    /// Create the (hidden) overlay window.
    pub fn create(app: &AppHandle, prefs: Prefs) -> tauri::Result<Arc<Overlay>> {
        let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay.html".into()))
            .title("Scout overlay")
            .transparent(true)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false)
            .resizable(false)
            .shadow(false)
            .inner_size(CARD.0, CARD.1)
            .build()?;
        #[cfg(windows)]
        let hwnd = window.hwnd().ok().map(|h| h.0 as usize as u64);
        #[cfg(not(windows))]
        let hwnd: Option<u64> = None;
        if let Some(h) = hwnd
            && let Err(e) = klip_platform::make_overlay(h)
        {
            tracing::warn!(error = %e, "overlay window style");
        }
        window.set_ignore_cursor_events(true)?;
        let pos_override = std::env::var("KLIP_OVERLAY_POS")
            .ok()
            .and_then(|s| crate::parse_pos(&s));
        Ok(Arc::new(Overlay {
            app: app.clone(),
            window,
            hwnd,
            state: Mutex::new(State {
                prefs,
                showing: Showing::Nothing,
                recording: false,
                game_window: None,
                recent: Vec::new(),
            }),
            generation: AtomicU64::new(0),
            next_card: AtomicU64::new(1),
            pos_override,
            no_focus: std::env::var_os("KLIP_NO_FOCUS").is_some(),
        }))
    }

    /// React to an engine event (on the event thread).
    pub fn on_event(self: &Arc<Self>, e: &Event) {
        match e {
            Event::SettingsChanged { settings } => {
                let prefs = Prefs::from_settings(settings);
                let (dot_off, showing) = {
                    let mut st = lock(&self.state);
                    st.prefs = prefs;
                    (!(st.prefs.mode == "full" && st.prefs.dot), st.showing)
                };
                if showing == Showing::Dot && dot_off {
                    self.hide();
                }
            }
            Event::GameStarted { game } => lock(&self.state).game_window = game.window,
            Event::GameStopped { .. } => lock(&self.state).game_window = None,
            Event::ClipSaved { game, window, clip } => {
                let name = game.clone().unwrap_or_else(|| "Desktop".into());
                let duration = clip.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
                let path = clip.get("path").and_then(Value::as_str).map(str::to_string);
                {
                    let mut st = lock(&self.state);
                    st.recent.insert(0, (name.clone(), duration));
                    st.recent.truncate(MAX_RECENT);
                    if window.is_some() {
                        st.game_window = *window;
                    }
                }
                self.clip_saved(&name, duration, path);
            }
            Event::OverlayPreview { game, duration_ms } => {
                let name = game.clone().unwrap_or_else(|| "Your game".into());
                self.clip_saved(&name, *duration_ms, None);
            }
            Event::Thumbnail { clip, data_url, .. } => {
                let _ = self.app.emit_to(
                    LABEL,
                    "overlay://show",
                    json!({ "kind": "thumb", "clip": clip, "data_url": data_url }),
                );
            }
            Event::RecordingStarted { .. } => {
                lock(&self.state).recording = true;
                self.show_dot_if_wanted();
            }
            Event::RecordingStopped { .. } | Event::RecorderStopped { .. } => {
                let showing = {
                    let mut st = lock(&self.state);
                    st.recording = false;
                    st.showing
                };
                if showing == Showing::Dot {
                    self.hide();
                }
            }
            Event::QuickMenu => self.menu(),
            _ => {}
        }
    }

    fn clip_saved(self: &Arc<Self>, name: &str, duration_ms: u64, clip: Option<String>) {
        let prefs = lock(&self.state).prefs.clone();
        if prefs.sound {
            crate::cue::play();
        }
        if prefs.notification {
            notify(&self.app, name, duration_ms);
        }
        if prefs.mode == "off" || klip_platform::exclusive_fullscreen_active() {
            return;
        }
        if lock(&self.state).showing == Showing::Menu {
            return;
        }
        let this_show = self.bump();
        let id = self.next_card.fetch_add(1, Ordering::Relaxed);
        self.place(CARD, false);
        let _ = self.app.emit_to(
            LABEL,
            "overlay://show",
            json!({
                "kind": "clip",
                "id": id,
                "game": name,
                "duration_ms": duration_ms,
                "clip": clip,
                "thumb": null,
                "seconds": prefs.seconds,
            }),
        );
        self.show(Showing::Card);
        let me = self.clone();
        self.after(Duration::from_secs(prefs.seconds), move || {
            if me.generation.load(Ordering::Relaxed) != this_show {
                return;
            }
            if !me.show_dot_if_wanted() {
                me.hide();
            }
        });
    }

    /// Full mode: the recording dot. Returns whether it is shown.
    fn show_dot_if_wanted(self: &Arc<Self>) -> bool {
        let (want, showing) = {
            let st = lock(&self.state);
            (
                st.recording && st.prefs.mode == "full" && st.prefs.dot,
                st.showing,
            )
        };
        if !want || matches!(showing, Showing::Card | Showing::Menu) {
            return false;
        }
        if klip_platform::exclusive_fullscreen_active() {
            return false;
        }
        self.bump();
        self.place(DOT, false);
        let _ = self
            .app
            .emit_to(LABEL, "overlay://show", json!({ "kind": "dot" }));
        self.show(Showing::Dot);
        true
    }

    /// Full mode: the quick menu (its hotkey).
    fn menu(self: &Arc<Self>) {
        let (prefs, recording, recent) = {
            let st = lock(&self.state);
            (st.prefs.clone(), st.recording, st.recent.clone())
        };
        if prefs.mode != "full" || klip_platform::exclusive_fullscreen_active() {
            return;
        }
        let this_show = self.bump();
        let game_name = lock(&self.state).recent.first().map(|(n, _)| n.clone());
        self.place(MENU, true);
        let _ = self.window.set_ignore_cursor_events(false);
        let _ = self.app.emit_to(
            LABEL,
            "overlay://show",
            json!({
                "kind": "menu",
                "game": game_name,
                "recording": recording,
                "clips": recent.iter().map(|(n, d)| json!({ "name": n, "duration_ms": d })).collect::<Vec<_>>(),
            }),
        );
        self.show(Showing::Menu);
        if !self.no_focus
            && let Some(h) = self.hwnd
        {
            klip_platform::focus_window(h);
        }
        let me = self.clone();
        self.after(MENU_TIMEOUT, move || {
            if me.generation.load(Ordering::Relaxed) == this_show {
                me.close_menu();
            }
        });
    }

    /// Close the quick menu and give the game its focus back.
    pub fn close_menu(self: &Arc<Self>) {
        let game = {
            let st = lock(&self.state);
            if st.showing != Showing::Menu {
                return;
            }
            st.game_window
        };
        self.bump();
        let _ = self.window.set_ignore_cursor_events(true);
        self.hide();
        if !self.no_focus
            && let Some(g) = game
        {
            klip_platform::focus_window(g);
        }
        self.show_dot_if_wanted();
    }

    fn bump(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    fn after(&self, d: Duration, f: impl FnOnce() + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("klip-overlay-timer".into())
            .spawn(move || {
                std::thread::sleep(d);
                crate::guard::guarded("overlay timer", f);
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "overlay timer");
        }
    }

    fn show(&self, what: Showing) {
        lock(&self.state).showing = what;
        match self.hwnd {
            Some(h) => {
                if let Err(e) = klip_platform::show_no_activate(h) {
                    tracing::warn!(error = %e, "showing the overlay");
                }
            }
            None => {
                let _ = self.window.show();
            }
        }
    }

    fn hide(&self) {
        lock(&self.state).showing = Showing::Nothing;
        // Shown with Win32 (`show_no_activate`), so hidden the same way: Tauri still
        // thinks the window is hidden and would skip its own `hide()`.
        match self.hwnd {
            Some(h) => {
                let _ = klip_platform::hide_window(h);
            }
            None => {
                let _ = self.window.hide();
            }
        }
        let _ = self
            .app
            .emit_to(LABEL, "overlay://show", json!({ "kind": "hide" }));
    }

    /// Size the window and put it in its corner (or the middle, for the menu) of the
    /// game's monitor.
    fn place(&self, (lw, lh): (f64, f64), centered: bool) {
        if let Some((x, y)) = self.pos_override {
            let _ = self
                .window
                .set_size(PhysicalSize::new(lw.round() as u32, lh.round() as u32));
            let _ = self.window.set_position(PhysicalPosition::new(x, y));
            return;
        }
        let game = lock(&self.state).game_window;
        let point = game
            .and_then(|h| klip_platform::window_info(h).ok())
            .map(|w| {
                (
                    w.client_x
                        .saturating_add(i32::try_from(w.client_width / 2).unwrap_or(0)),
                    w.client_y
                        .saturating_add(i32::try_from(w.client_height / 2).unwrap_or(0)),
                )
            });
        let monitors = self.window.available_monitors().unwrap_or_default();
        let monitor = point
            .and_then(|(px, py)| {
                monitors.iter().find(|m| {
                    let (p, s) = (m.position(), m.size());
                    px >= p.x
                        && py >= p.y
                        && i64::from(px) < i64::from(p.x).saturating_add(i64::from(s.width))
                        && i64::from(py) < i64::from(p.y).saturating_add(i64::from(s.height))
                })
            })
            .cloned()
            .or_else(|| self.window.primary_monitor().ok().flatten());
        let Some(m) = monitor else { return };
        let scale = m.scale_factor();
        let (w, h) = ((lw * scale).round() as u32, (lh * scale).round() as u32);
        let (p, s) = (m.position(), m.size());
        let (x, y) = if centered {
            (
                p.x.saturating_add(i32::try_from(s.width.saturating_sub(w) / 2).unwrap_or(0)),
                p.y.saturating_add(i32::try_from(s.height.saturating_sub(h) / 3).unwrap_or(0)),
            )
        } else {
            let corner = lock(&self.state).prefs.corner.clone();
            corner_position(
                &corner,
                (p.x, p.y, s.width, s.height),
                (w, h),
                (MARGIN * scale).round() as u32,
            )
        };
        let _ = self.window.set_size(PhysicalSize::new(w, h));
        let _ = self.window.set_position(PhysicalPosition::new(x, y));
    }
}

fn notify(app: &AppHandle, game: &str, duration_ms: u64) {
    use tauri_plugin_notification::NotificationExt;
    let secs = duration_ms / 1000;
    if let Err(e) = app
        .notification()
        .builder()
        .title("Clip saved")
        .body(format!("{game} · {secs} s"))
        .show()
    {
        tracing::debug!(error = %e, "notification");
    }
}

/// The overlay's state, for `Manager::state`.
pub struct OverlayHandle(pub Arc<Overlay>);

/// Close the quick menu (called by the overlay page).
pub fn close(app: &AppHandle) {
    if let Some(h) = app.try_state::<OverlayHandle>() {
        h.0.close_menu();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners() {
        let m = (2560, 0, 2560, 1440);
        assert_eq!(corner_position("top-right", m, (392, 100), 18), (4710, 18));
        assert_eq!(corner_position("top-left", m, (392, 100), 18), (2578, 18));
        assert_eq!(
            corner_position("bottom-left", m, (392, 100), 18),
            (2578, 1322)
        );
        assert_eq!(
            corner_position("bottom-right", m, (392, 100), 18),
            (4710, 1322)
        );
        assert_eq!(corner_position("nonsense", m, (392, 100), 18), (4710, 18));
    }

    #[test]
    fn prefs_from_settings() {
        let p = Prefs::from_settings(
            &json!({"overlay": {"mode": "full", "seconds": 40, "sound": false}}),
        );
        assert_eq!(p.mode, "full");
        assert_eq!(p.seconds, 10, "clamped");
        assert!(!p.sound);
        assert_eq!(p.corner, "top-right");
        let d = Prefs::from_settings(&json!({}));
        assert_eq!((d.mode.as_str(), d.seconds, d.sound), ("minimal", 3, true));
    }
}
