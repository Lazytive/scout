//! What the engine reports: one JSON object per event (`{"event":"clip_saved",…}`),
//! delivered to every subscriber. Subscribers get a bounded queue; a subscriber that
//! stops reading loses events instead of growing memory.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use serde_json::Value;

use crate::games::DetectedGame;

/// Events queued per subscriber before new ones are dropped.
pub const QUEUE: usize = 256;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The engine is running.
    Ready {
        version: &'static str,
        /// How the settings file loaded (`missing`, `loaded`, `repaired`, `reset`).
        settings: &'static str,
        /// What was repaired in it.
        issues: Vec<String>,
        /// Control channel port, if on.
        port: Option<u16>,
    },
    GameStarted {
        game: DetectedGame,
    },
    GameStopped {
        game: DetectedGame,
    },
    /// The replay buffer runs (for `game`, or the desktop when `None`).
    RecorderStarted {
        game: Option<String>,
        encoder: Value,
        width: Value,
        height: Value,
        fps: u32,
        buffer_ms: i64,
        clip_dir: String,
        audio: Value,
    },
    RecorderStopped {
        reason: String,
    },
    RecorderFailed {
        game: Option<String>,
        error: String,
        /// Seconds until the next try.
        retry_secs: u64,
    },
    ClipSaved {
        game: Option<String>,
        /// Window of the game (for the overlay's monitor).
        window: Option<u64>,
        clip: Value,
    },
    RecordingStarted {
        path: String,
    },
    RecordingStopped {
        summary: Value,
    },
    Bookmark {
        result: Value,
    },
    /// A hotkey's action failed (commands report their errors to the caller instead).
    ActionFailed {
        action: String,
        code: String,
        message: String,
    },
    /// The quick-menu hotkey was pressed.
    QuickMenu,
    /// Microphone meter (`audio.meter`), about 15 per second.
    MicLevel {
        peak_db: f32,
        rms_db: f32,
    },
    /// Hotkeys after (re)binding; each with its error if it isn't active.
    Hotkeys {
        bound: Vec<Value>,
    },
    SettingsChanged {
        settings: Value,
    },
    /// A custom theme was saved, imported or deleted (M10).
    ThemesChanged {
        id: String,
    },
    /// A saved clip's thumbnail (JPEG, 320 px wide), also cached for the library.
    Thumbnail {
        clip: String,
        path: Option<String>,
        data_url: String,
    },
    /// `overlay.preview`: show the pop-up as if a clip had been saved.
    OverlayPreview {
        game: Option<String>,
        duration_ms: u64,
    },
    /// Clips removed to stay under the storage cap.
    StorageCleaned {
        report: Value,
    },
    /// Clips were renamed, tagged, starred or deleted through Klip (`reason`).
    LibraryChanged {
        reason: String,
    },
    /// A clip's thumbnails were made (versions: modification times in ms; `None`: still
    /// missing). `failed`: they could not be made and won't be retried.
    ThumbsReady {
        key: String,
        poster: Option<u64>,
        strip: Option<u64>,
        failed: bool,
    },
    /// An export (or the editor's proxy) moved on: `pass` and frames of this pass.
    ExportProgress {
        id: u64,
        key: String,
        preset: String,
        pass: u32,
        frames_done: u64,
        frames_total: u64,
    },
    /// An export ended: `path` and `summary` (`klip_export::ExportSummary`) when `ok`,
    /// `error` otherwise (cancel included).
    ExportDone {
        id: u64,
        key: String,
        preset: String,
        ok: bool,
        path: Option<String>,
        error: Option<String>,
        summary: Value,
    },
    /// Recordings a killed recorder left, finalised at start.
    Recovered {
        path: String,
        result: Value,
    },
    Warning {
        message: String,
    },
}

impl Event {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Fan-out to subscribers.
#[derive(Default)]
pub(crate) struct Bus {
    subs: Mutex<Vec<SyncSender<Event>>>,
}

impl Bus {
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = sync_channel(QUEUE);
        self.add(tx);
        rx
    }

    pub fn add(&self, tx: SyncSender<Event>) {
        self.subs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tx);
    }

    pub fn emit(&self, e: &Event) {
        let mut subs = self.subs.lock().unwrap_or_else(PoisonError::into_inner);
        subs.retain(|tx| match tx.try_send(e.clone()) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}
