//! Linux: the app starts and its overlay pops up. Regression test for the start-up abort
//! found on the Bazzite pass (2026-10-13): the hidden overlay window was made
//! click-through before GTK had made its surface, and tao panicked inside a GTK callback
//! (an abort, ~0.2 s after start). The app's window and the overlay card appear on the
//! desktop for a few seconds (Wayland lets no app place itself off-screen).

#![cfg(target_os = "linux")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Kill {
    fn assert_running(&mut self, when: &str) {
        if let Some(status) = self.0.try_wait().unwrap() {
            panic!("the app exited {when}: {status}");
        }
    }
}

fn connect(app: &mut Kill, discovery: &Path) -> klip_control::Client {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        app.assert_running("while starting");
        if let Ok(mut c) = klip_control::Client::connect(discovery, Duration::from_secs(1))
            && c.call("ping", Value::Null).is_ok()
        {
            return c;
        }
        assert!(Instant::now() < deadline, "the app did not start");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "e2e: runs the app on the desktop; run via `cargo xtask e2e`"]
fn app_starts_and_shows_the_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    let discovery = tmp.path().join("control.json");
    std::fs::write(
        &settings,
        json!({
            "app": {"control_channel": true, "first_run_done": true, "start_minimized": true},
            "clips": {"folder": tmp.path().join("clips")},
            "hotkeys": [],
            "games": {"buffer": "off"},
            "overlay": {"mode": "minimal", "seconds": 2, "sound": false, "notification": false},
        })
        .to_string(),
    )
    .unwrap();
    let mut app = Kill(
        Command::new(env!("CARGO_BIN_EXE_klip-app"))
            .env("KLIP_SETTINGS", &settings)
            .env("KLIP_DISCOVERY", &discovery)
            .env("KLIP_NO_FOCUS", "1")
            .spawn()
            .unwrap(),
    );
    let mut c = connect(&mut app, &discovery);
    // The overlay is made during start-up, after the control channel answers.
    std::thread::sleep(Duration::from_secs(3));
    app.assert_running("after start-up");
    c.call("overlay.preview", Value::Null).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    app.assert_running("showing the overlay");
    c.call("ping", Value::Null).unwrap();
    // Hidden again by its timer, then shown a second time.
    std::thread::sleep(Duration::from_secs(3));
    c.call("overlay.preview", Value::Null).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    app.assert_running("showing the overlay again");
    c.call("ping", Value::Null).unwrap();
}
