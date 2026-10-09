//! Windows hotkeys, `press_keys`, `process_path` and `list_windows`.

use std::sync::mpsc;
use std::time::Duration;

use crate::{GlobalShortcuts, ShortcutSpec, list_windows, press_keys, process_path};

fn spec(id: &str, keys: Option<&str>) -> ShortcutSpec {
    ShortcutSpec {
        id: id.into(),
        description: String::new(),
        preferred_trigger: keys.map(Into::into),
    }
}

#[test]
fn every_settings_key_maps_to_a_virtual_key() {
    for key in klip_settings::hotkey::all_keys() {
        let h = klip_settings::Hotkey {
            ctrl: true,
            alt: false,
            shift: true,
            logo: false,
            key,
        };
        let t = crate::keymap::parse_trigger(&h.to_trigger()).unwrap();
        assert_eq!(t.canonical, h.to_trigger());
    }
}

#[test]
fn conflicts_and_bad_keys_are_reported() {
    let specs = [
        spec("a", Some("CTRL+ALT+SHIFT+F23")),
        spec("b", Some("ctrl+alt+shift+f23")),
        spec("c", Some("CTRL+Banana")),
        spec("d", None),
    ];
    let s = GlobalShortcuts::bind(&specs, Box::new(|_| {})).unwrap();
    let b = s.bound();
    assert_eq!(b[0].error, None);
    assert_eq!(b[0].trigger, "CTRL+ALT+SHIFT+F23");
    assert_eq!(b[1].error.as_deref(), Some("already used by another app"));
    assert!(b[2].error.as_ref().unwrap().contains("unknown key"));
    assert_eq!(b[3].error.as_deref(), Some("no keys set"));
    drop(s);
    // Dropping unregistered them: binding again works.
    let s = GlobalShortcuts::bind(&specs[..1], Box::new(|_| {})).unwrap();
    assert_eq!(s.bound()[0].error, None);
}

#[test]
#[ignore = "hardware: presses F24 system-wide (no keyboard has it)"]
fn a_pressed_hotkey_fires() {
    let (tx, rx) = mpsc::channel();
    let s = GlobalShortcuts::bind(
        &[spec("clip.save", Some("F24"))],
        Box::new(move |id| {
            let _ = tx.send(id.to_string());
        }),
    )
    .unwrap();
    assert_eq!(s.bound()[0].error, None, "F24 is taken by another app");
    for _ in 0..2 {
        press_keys("F24").unwrap();
        let id = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(id, "clip.save");
    }
    drop(s);
    press_keys("F24").unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
}

#[test]
fn own_process_path_and_windows() {
    let p = process_path(std::process::id()).unwrap();
    let me = std::env::current_exe().unwrap();
    assert!(p.is_absolute());
    assert_eq!(
        p.file_name().unwrap().to_string_lossy().to_lowercase(),
        me.file_name().unwrap().to_string_lossy().to_lowercase()
    );
    assert!(process_path(u32::MAX - 3).is_err());
    assert!(list_windows().is_ok());
}
