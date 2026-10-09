//! Windows global hotkeys: `RegisterHotKey` on a thread of our own with a message loop.
//! The OS sends `WM_HOTKEY` to that thread whichever app has focus; nothing is injected
//! into or hooked in any other process. The key combination is consumed (the focused app
//! doesn't see it), and a combination another app holds can't be registered. Every
//! `unsafe` block is a call into a documented user32 API with arguments owned here.

#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT, WM_USER,
};
use windows::core::HRESULT;

use crate::PlatformError;
use crate::keymap::parse_trigger;
use crate::shortcuts::{BoundShortcut, ShortcutSpec};

/// `ERROR_HOTKEY_ALREADY_REGISTERED`.
const ALREADY_REGISTERED: u32 = 1409;

/// The hotkey thread; dropping it unregisters the hotkeys and ends the thread.
pub(crate) struct Session {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(j) = self.join.take() {
            // A finished thread's id may belong to another thread by now: only post to a
            // live one.
            if !j.is_finished() {
                // SAFETY: plain message post to our own thread id; failure is harmless.
                let _ =
                    unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            }
            let _ = j.join();
        }
    }
}

type Planned = (String, Result<(u32, u32, String), String>);

pub(crate) fn bind(
    specs: &[ShortcutSpec],
    on_activated: Box<dyn Fn(&str) + Send>,
) -> Result<(Session, Vec<BoundShortcut>), PlatformError> {
    let planned: Vec<Planned> = specs
        .iter()
        .map(|s| {
            let parsed = match &s.preferred_trigger {
                None => Err("no keys set".to_string()),
                Some(t) => parse_trigger(t).map(|t| (t.mods, t.vk, t.canonical)),
            };
            (s.id.clone(), parsed)
        })
        .collect();
    let (tx, rx) = mpsc::channel();
    let join = std::thread::Builder::new()
        .name("klip-hotkeys".into())
        .spawn(move || run(planned, on_activated, &tx))
        .map_err(|e| PlatformError::Os(format!("hotkey thread: {e}")))?;
    let (thread_id, bound) = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| PlatformError::Os("hotkey thread did not start".into()))?;
    Ok((
        Session {
            thread_id,
            join: Some(join),
        },
        bound,
    ))
}

fn describe(e: &windows::core::Error) -> String {
    if e.code() == HRESULT::from_win32(ALREADY_REGISTERED) {
        "already used by another app".into()
    } else {
        format!("could not register: {e}")
    }
}

fn run(
    planned: Vec<Planned>,
    on_activated: Box<dyn Fn(&str) + Send>,
    tx: &mpsc::Sender<(u32, Vec<BoundShortcut>)>,
) {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid out pointer. This creates the thread's message queue before
    // its id is handed out, so a WM_QUIT posted from `Drop` can't be lost.
    let _ = unsafe { PeekMessageW(&raw mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };
    // SAFETY: no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut registered: Vec<(i32, String)> = Vec::new();
    let mut bound = Vec::with_capacity(planned.len());
    for (n, (id, parsed)) in (1i32..).zip(planned) {
        match parsed {
            Ok((mods, vk, trigger)) => {
                let flags = HOT_KEY_MODIFIERS(mods | MOD_NOREPEAT.0);
                // SAFETY: registers for this thread (no window); `n` is unique per thread.
                match unsafe { RegisterHotKey(None, n, flags, vk) } {
                    Ok(()) => {
                        registered.push((n, id.clone()));
                        bound.push(BoundShortcut {
                            id,
                            trigger,
                            error: None,
                        });
                    }
                    Err(e) => bound.push(BoundShortcut {
                        id,
                        trigger,
                        error: Some(describe(&e)),
                    }),
                }
            }
            Err(e) => bound.push(BoundShortcut {
                id,
                trigger: String::new(),
                error: Some(e),
            }),
        }
    }
    if tx.send((thread_id, bound)).is_ok() {
        loop {
            // SAFETY: `msg` is a valid out pointer; this thread's queue only.
            let r = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
            // 0: WM_QUIT, -1: error.
            if r.0 <= 0 {
                break;
            }
            if msg.message == WM_HOTKEY {
                let n = i32::try_from(msg.wParam.0).unwrap_or(0);
                if let Some((_, id)) = registered.iter().find(|(h, _)| *h == n) {
                    let _ = catch_unwind(AssertUnwindSafe(|| on_activated(id)));
                }
            }
        }
    }
    for (n, _) in registered {
        // SAFETY: unregisters a hotkey this thread registered.
        let _ = unsafe { UnregisterHotKey(None, n) };
    }
}

/// Press and release `trigger` (modifiers down, key down, key up, modifiers up) with
/// `SendInput`, as if typed. For tests and automation.
pub(crate) fn press(trigger: &str) -> Result<(), PlatformError> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VIRTUAL_KEY,
    };

    use crate::keymap::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

    let t = parse_trigger(trigger).map_err(PlatformError::Os)?;
    let mut keys: Vec<u16> = [
        (MOD_CONTROL, 0x11u16),
        (MOD_ALT, 0x12),
        (MOD_SHIFT, 0x10),
        (MOD_WIN, 0x5B),
    ]
    .iter()
    .filter(|(m, _)| t.mods & m != 0)
    .map(|(_, vk)| *vk)
    .collect();
    keys.push(u16::try_from(t.vk).map_err(|_| PlatformError::Os("bad key".into()))?);
    let input = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let mut inputs: Vec<INPUT> = keys.iter().map(|vk| input(*vk, false)).collect();
    inputs.extend(keys.iter().rev().map(|vk| input(*vk, true)));
    let size = i32::try_from(std::mem::size_of::<INPUT>()).unwrap_or(0);
    // SAFETY: `inputs` is a valid slice of initialised keyboard INPUTs of `size` bytes each.
    let sent = unsafe { SendInput(&inputs, size) };
    if usize::try_from(sent).unwrap_or(0) == inputs.len() {
        Ok(())
    } else {
        Err(PlatformError::Os("SendInput was blocked".into()))
    }
}
