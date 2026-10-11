//! The tray icon: its picture (drawn here, so there is no image asset to license) shows
//! whether Scout waits for a game, buffers or records; its menu saves a clip, starts or
//! stops a recording, opens the window or quits.

use std::sync::{Arc, Mutex, PoisonError};

use klip_control::{Handler, commands};
use klip_engine::Event;
use serde_json::Value;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Runtime};

/// Tray icon size in pixels.
const SIZE: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Idle,
    Buffering,
    Recording,
}

/// What the tray shows; updated from engine events.
#[derive(Default)]
pub struct TrayState {
    game: Option<String>,
    buffering: bool,
    recording: bool,
}

impl TrayState {
    pub fn look(&self) -> Look {
        match (self.buffering, self.recording) {
            (_, true) => Look::Recording,
            (true, false) => Look::Buffering,
            (false, false) => Look::Idle,
        }
    }

    pub fn tooltip(&self) -> String {
        let who = self.game.as_deref().unwrap_or("the desktop");
        match self.look() {
            Look::Recording => format!("Scout: recording {who}"),
            Look::Buffering => format!("Scout: buffering {who}"),
            Look::Idle => "Scout: waiting for a game".into(),
        }
    }

    /// Apply an event; returns whether the tray changed.
    pub fn apply(&mut self, e: &Event) -> bool {
        let before = (self.game.clone(), self.buffering, self.recording);
        match e {
            Event::GameStarted { game } => self.game = Some(game.name.clone()),
            Event::GameStopped { .. } => self.game = None,
            Event::RecorderStarted { .. } => self.buffering = true,
            Event::RecorderStopped { .. } => {
                self.buffering = false;
                self.recording = false;
            }
            Event::RecordingStarted { .. } => self.recording = true,
            Event::RecordingStopped { .. } => self.recording = false,
            _ => {}
        }
        before != (self.game.clone(), self.buffering, self.recording)
    }
}

/// RGBA pixels of the tray icon: a rounded square (grey when idle, the accent blue when
/// buffering) with a white play mark, plus a red dot while recording. 4x4 supersampled.
pub fn render(look: Look) -> Vec<u8> {
    let size = SIZE as f32;
    let base: [f32; 3] = match look {
        Look::Idle => [0x6b as f32, 0x6b as f32, 0x6b as f32],
        Look::Buffering | Look::Recording => [0x34 as f32, 0x90 as f32, 0xf7 as f32],
    };
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for py in 0..SIZE {
        for px in 0..SIZE {
            let mut acc = [0f32; 4];
            for sy in 0..4u32 {
                for sx in 0..4u32 {
                    let x = px as f32 + (sx as f32 + 0.5) / 4.0;
                    let y = py as f32 + (sy as f32 + 0.5) / 4.0;
                    let c = shade(x, y, size, base, look);
                    for (a, v) in acc.iter_mut().zip(c) {
                        *a += v / 16.0;
                    }
                }
            }
            // Store premultiplied-free RGBA: colour divided by coverage.
            let alpha = acc[3];
            for v in &acc[..3] {
                let c = if alpha > 0.0 { v / alpha } else { 0.0 };
                out.push(c.round().clamp(0.0, 255.0) as u8);
            }
            out.push((alpha * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    out
}

/// Colour (premultiplied by coverage) and coverage of one sample.
fn shade(x: f32, y: f32, size: f32, base: [f32; 3], look: Look) -> [f32; 4] {
    let inset = 2.0;
    let r = 7.0;
    // Rounded square.
    let (lo, hi) = (inset, size - inset);
    let cx = x.clamp(lo + r, hi - r);
    let cy = y.clamp(lo + r, hi - r);
    let inside_square = x >= lo && x <= hi && y >= lo && y <= hi && (x - cx).hypot(y - cy) <= r;
    if look == Look::Recording {
        let (dx, dy) = (x - 24.5, y - 24.5);
        let d = dx.hypot(dy);
        if d <= 6.0 {
            return [0xff as f32, 0x4b as f32, 0x4b as f32, 1.0];
        }
        if d <= 7.5 {
            // A dark ring separates the dot from the square.
            return [0x1d as f32, 0x1d as f32, 0x1d as f32, 1.0];
        }
    }
    if !inside_square {
        return [0.0; 4];
    }
    // Play mark: a triangle pointing right.
    let (tx0, tx1, ty0, ty1) = (12.0, 22.5, 9.5, 22.5);
    let mid = (ty0 + ty1) / 2.0;
    let half = (ty1 - ty0) / 2.0;
    let in_triangle =
        x >= tx0 && x <= tx1 && (y - mid).abs() <= half * (1.0 - (x - tx0) / (tx1 - tx0));
    let rgb = if in_triangle { [255.0; 3] } else { base };
    [rgb[0], rgb[1], rgb[2], 1.0]
}

fn image(look: Look) -> Image<'static> {
    Image::new_owned(render(look), SIZE, SIZE)
}

/// Run a command off the UI thread (a save can take a moment).
fn job(handler: &Arc<dyn Handler>, cmd: &'static str) {
    let h = handler.clone();
    let spawned = std::thread::Builder::new()
        .name("klip-tray-job".into())
        .spawn(move || {
            if let Err(e) = h.handle(cmd, &Value::Null) {
                tracing::info!(cmd, error = %e.message, "tray action");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "no thread for a tray action");
    }
}

pub fn create<R: Runtime>(
    app: &AppHandle<R>,
    handler: Arc<dyn Handler>,
    state: Arc<Mutex<TrayState>>,
    show_main: fn(&AppHandle<R>),
) -> tauri::Result<TrayIcon<R>> {
    let save = MenuItem::with_id(app, "save", "Save clip", true, None::<&str>)?;
    let record = MenuItem::with_id(app, "record", "Start or stop recording", true, None::<&str>)?;
    let bookmark = MenuItem::with_id(app, "bookmark", "Bookmark", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Scout", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Scout", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&save, &record, &bookmark, &sep, &open, &quit])?;
    let tooltip = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .tooltip();
    TrayIconBuilder::with_id("klip")
        .icon(image(Look::Idle))
        .tooltip(tooltip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, e| match e.id.as_ref() {
            "save" => job(&handler, commands::CLIP_SAVE),
            "record" => job(&handler, commands::SESSION_TOGGLE),
            "bookmark" => job(&handler, commands::BOOKMARK_ADD),
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(move |tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)
}

/// Update the tray after an event.
pub fn update<R: Runtime>(tray: &TrayIcon<R>, state: &Mutex<TrayState>, e: &Event) {
    let mut s = state.lock().unwrap_or_else(PoisonError::into_inner);
    if !s.apply(e) {
        return;
    }
    let (look, tip) = (s.look(), s.tooltip());
    drop(s);
    if let Err(err) = tray.set_icon(Some(image(look))) {
        tracing::warn!(error = %err, "tray icon");
    }
    let _ = tray.set_tooltip(Some(tip));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn icons_show_the_state() {
        let idle = render(Look::Idle);
        let buf = render(Look::Buffering);
        let rec = render(Look::Recording);
        for img in [&idle, &buf, &rec] {
            assert_eq!(img.len(), (SIZE * SIZE * 4) as usize);
            assert_eq!(px(img, 0, 0)[3], 0, "transparent corner");
            assert_eq!(px(img, 15, 16), [255, 255, 255, 255], "white play mark");
        }
        assert_eq!(px(&idle, 6, 16), [0x6b, 0x6b, 0x6b, 255]);
        assert_eq!(px(&buf, 6, 16), [0x34, 0x90, 0xf7, 255]);
        assert_eq!(px(&rec, 24, 24), [0xff, 0x4b, 0x4b, 255]);
        assert_ne!(px(&buf, 24, 24), [0xff, 0x4b, 0x4b, 255]);
    }

    #[test]
    fn state_follows_events() {
        let game = klip_engine::games::DetectedGame {
            pid: 1,
            exe: "g.exe".into(),
            key: "g.exe".into(),
            name: "Game".into(),
            window: None,
            source: klip_engine::games::Source::User,
            foreground: true,
        };
        let mut s = TrayState::default();
        assert_eq!(s.tooltip(), "Scout: waiting for a game");
        assert!(s.apply(&Event::GameStarted { game: game.clone() }));
        assert!(s.apply(&Event::RecorderStarted {
            game: Some("Game".into()),
            encoder: Value::Null,
            width: Value::Null,
            height: Value::Null,
            fps: 60,
            buffer_ms: 1,
            clip_dir: String::new(),
            audio: Value::Null,
        }));
        assert_eq!(s.look(), Look::Buffering);
        assert_eq!(s.tooltip(), "Scout: buffering Game");
        assert!(s.apply(&Event::RecordingStarted {
            path: String::new()
        }));
        assert_eq!(s.look(), Look::Recording);
        assert!(!s.apply(&Event::Warning {
            message: String::new()
        }));
        assert!(s.apply(&Event::RecorderStopped {
            reason: String::new()
        }));
        assert!(s.apply(&Event::GameStopped { game }));
        assert_eq!(s.look(), Look::Idle);
    }
}
