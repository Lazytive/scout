//! Video capture supervision (M3.6): keep video coming when window capture can't see a
//! game.
//!
//! Window capture (WGC) shows nothing new for a game in exclusive fullscreen, for some
//! protected or misbehaving windows, and when the capture session dies. The supervisor
//! looks at the capture twice a second and switches between capturing the window and
//! capturing the monitor it covers:
//! - window covers its monitor and is the foreground app in exclusive fullscreen →
//!   monitor;
//! - window covers its monitor and no frame came for [`STALL`] → monitor;
//! - window capture failed (error) while the window still exists → monitor;
//! - on the monitor, the window no longer covers it (back to windowed) → window;
//! - on the monitor, still nothing for [`STALL`] in exclusive fullscreen → DXGI Desktop
//!   Duplication, which sees exclusive-fullscreen output.
//!
//! The decision is a pure function ([`decide`]) so its rules are unit-tested; the restart
//! keeps the encoder and output size, so the replay track (and buffer) carries on.

use std::time::Duration;

/// No frame for this long counts as a stall.
pub const STALL: Duration = Duration::from_secs(3);

/// Least time between two switches (no flapping).
pub const MIN_DWELL: Duration = Duration::from_secs(5);

/// What video is captured right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoMode {
    /// The window the user chose.
    Window { hwnd: u64 },
    /// The monitor under the chosen window, because the window can't be captured.
    /// `dxgi`: through DXGI Desktop Duplication instead of WGC.
    WindowOnMonitor { hwnd: u64, monitor: u64, dxgi: bool },
    /// A monitor the user chose: never switched.
    Monitor { monitor: u64 },
}

/// What the supervisor sees on one tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// Time since the capture last produced a frame.
    pub stalled: Duration,
    /// The capture reported an error since it started.
    pub failed: bool,
    pub window_exists: bool,
    pub minimized: bool,
    /// The window's client area covers its whole monitor.
    pub covers_monitor: bool,
    /// The monitor the window is (mostly) on.
    pub window_monitor: Option<u64>,
    /// The window has the keyboard focus.
    pub foreground: bool,
    /// The foreground app runs Direct3D exclusive fullscreen.
    pub exclusive: bool,
}

/// What to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Keep,
    Switch { to: VideoMode, reason: &'static str },
}

/// The supervisor's rules (see the module docs).
pub fn decide(mode: VideoMode, o: &Observation) -> Action {
    let switch = |to, reason| Action::Switch { to, reason };
    match mode {
        VideoMode::Monitor { .. } => Action::Keep,
        VideoMode::Window { hwnd } => {
            let Some(monitor) = o.window_monitor else {
                return Action::Keep;
            };
            if !o.window_exists || o.minimized {
                return Action::Keep;
            }
            let to = VideoMode::WindowOnMonitor {
                hwnd,
                monitor,
                dxgi: false,
            };
            if o.covers_monitor && o.foreground && o.exclusive {
                switch(to, "exclusive fullscreen")
            } else if o.failed {
                switch(to, "window capture failed")
            } else if o.covers_monitor && o.stalled >= STALL {
                switch(to, "window capture stalled")
            } else {
                Action::Keep
            }
        }
        VideoMode::WindowOnMonitor {
            hwnd,
            monitor,
            dxgi,
        } => {
            if !o.window_exists || o.minimized {
                return Action::Keep;
            }
            let fullscreen = o.foreground && o.exclusive;
            if !o.covers_monitor && !fullscreen {
                return switch(VideoMode::Window { hwnd }, "window left fullscreen");
            }
            if let Some(m) = o.window_monitor
                && m != monitor
            {
                return switch(
                    VideoMode::WindowOnMonitor {
                        hwnd,
                        monitor: m,
                        dxgi,
                    },
                    "window moved to another monitor",
                );
            }
            if !dxgi && fullscreen && (o.stalled >= STALL || o.failed) {
                return switch(
                    VideoMode::WindowOnMonitor {
                        hwnd,
                        monitor,
                        dxgi: true,
                    },
                    "monitor capture stalled in exclusive fullscreen",
                );
            }
            Action::Keep
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u64 = 0x10;
    const M: u64 = 0x20;

    fn fullscreen_window() -> Observation {
        Observation {
            window_exists: true,
            covers_monitor: true,
            window_monitor: Some(M),
            ..Default::default()
        }
    }

    fn target(a: Action) -> Option<VideoMode> {
        match a {
            Action::Keep => None,
            Action::Switch { to, .. } => Some(to),
        }
    }

    const ON_MONITOR: VideoMode = VideoMode::WindowOnMonitor {
        hwnd: W,
        monitor: M,
        dxgi: false,
    };

    #[test]
    fn window_capture_moves_to_the_monitor_when_it_cannot_see_the_game() {
        let window = VideoMode::Window { hwnd: W };
        // A healthy fullscreen (borderless) window stays on window capture.
        assert_eq!(decide(window, &fullscreen_window()), Action::Keep);
        // Exclusive fullscreen, foreground: monitor at once.
        let exclusive = Observation {
            foreground: true,
            exclusive: true,
            ..fullscreen_window()
        };
        assert_eq!(target(decide(window, &exclusive)), Some(ON_MONITOR));
        // Exclusive fullscreen of *another* app: not ours, keep.
        let other = Observation {
            foreground: false,
            exclusive: true,
            ..fullscreen_window()
        };
        assert_eq!(decide(window, &other), Action::Keep);
        // Stalled while covering the monitor: monitor.
        let stalled = Observation {
            stalled: STALL,
            ..fullscreen_window()
        };
        assert_eq!(target(decide(window, &stalled)), Some(ON_MONITOR));
        // Stalled but windowed (static content, e.g. a menu): keep.
        let windowed = Observation {
            covers_monitor: false,
            ..stalled.clone()
        };
        assert_eq!(decide(window, &windowed), Action::Keep);
        // Capture failed while the window exists: monitor, even windowed.
        let failed = Observation {
            failed: true,
            covers_monitor: false,
            ..fullscreen_window()
        };
        assert_eq!(target(decide(window, &failed)), Some(ON_MONITOR));
    }

    #[test]
    fn gone_or_minimised_windows_change_nothing() {
        let window = VideoMode::Window { hwnd: W };
        for o in [
            Observation {
                window_exists: false,
                failed: true,
                ..fullscreen_window()
            },
            Observation {
                minimized: true,
                stalled: STALL * 10,
                ..fullscreen_window()
            },
            Observation {
                window_monitor: None,
                failed: true,
                ..fullscreen_window()
            },
        ] {
            assert_eq!(decide(window, &o), Action::Keep, "{o:?}");
            assert_eq!(decide(ON_MONITOR, &o), Action::Keep, "{o:?}");
        }
    }

    #[test]
    fn monitor_capture_returns_to_the_window_and_escalates_to_dxgi() {
        // Still fullscreen: stay.
        assert_eq!(decide(ON_MONITOR, &fullscreen_window()), Action::Keep);
        // Windowed again: back to the window.
        let windowed = Observation {
            covers_monitor: false,
            ..fullscreen_window()
        };
        assert_eq!(
            target(decide(ON_MONITOR, &windowed)),
            Some(VideoMode::Window { hwnd: W })
        );
        // Moved to another monitor.
        let moved = Observation {
            window_monitor: Some(0x30),
            ..fullscreen_window()
        };
        assert_eq!(
            target(decide(ON_MONITOR, &moved)),
            Some(VideoMode::WindowOnMonitor {
                hwnd: W,
                monitor: 0x30,
                dxgi: false
            })
        );
        // Exclusive fullscreen and WGC monitor capture sees nothing: DXGI.
        let blind = Observation {
            foreground: true,
            exclusive: true,
            stalled: STALL,
            ..fullscreen_window()
        };
        let dxgi = VideoMode::WindowOnMonitor {
            hwnd: W,
            monitor: M,
            dxgi: true,
        };
        assert_eq!(target(decide(ON_MONITOR, &blind)), Some(dxgi));
        // Already on DXGI: nothing further to try.
        assert_eq!(decide(dxgi, &blind), Action::Keep);
        // A stall without exclusive fullscreen (static picture) doesn't escalate.
        let still = Observation {
            stalled: STALL * 3,
            ..fullscreen_window()
        };
        assert_eq!(decide(ON_MONITOR, &still), Action::Keep);
    }

    #[test]
    fn user_chosen_monitors_are_never_switched() {
        let o = Observation {
            failed: true,
            stalled: STALL * 10,
            foreground: true,
            exclusive: true,
            ..fullscreen_window()
        };
        assert_eq!(decide(VideoMode::Monitor { monitor: M }, &o), Action::Keep);
    }
}
