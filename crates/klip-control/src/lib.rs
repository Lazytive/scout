//! "Everything is a command": command ids, the JSON-lines protocol, and a loopback
//! control channel with token auth (ADR 0006).
//!
//! Wire format, one JSON object per line:
//! - first line from the client: `{"auth":"<hex token>"}`
//! - request: `{"id":1,"cmd":"clip.save","args":{"last_ms":30000}}`
//! - response: `{"id":1,"ok":true,"result":{…}}` or
//!   `{"id":1,"ok":false,"error":{"code":"…","message":"…"}}`

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod server;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use server::{Client, ClientError, Server, ServerError, read_line_capped};

/// Command ids. The UI, CLI and control channel all use these.
pub mod commands {
    /// Save the last N ms of the replay buffer. Args: `{"last_ms": u64}`.
    pub const CLIP_SAVE: &str = "clip.save";
    /// Recorder status (buffered time, bytes, encoder, output size).
    pub const RECORDER_STATUS: &str = "recorder.status";
    /// Stop the recorder process.
    pub const RECORDER_STOP: &str = "recorder.stop";
    /// Liveness check; returns `{"pong":true}`.
    pub const PING: &str = "ping";
    /// Start a full-session recording; returns `{"recording":true,"path":…}`.
    pub const SESSION_START: &str = "session.start";
    /// Stop it and finalise; returns `{"recording":false,"summary":{"files":[…]}}`.
    pub const SESSION_STOP: &str = "session.stop";
    /// Start or stop (what a hotkey does).
    pub const SESSION_TOGGLE: &str = "session.toggle";
    /// Mark this moment in the recording and in clips that cover it.
    pub const BOOKMARK_ADD: &str = "bookmark.add";
    /// Engine state: followed game, replay buffer, hotkeys, last error (ADR 0014).
    pub const ENGINE_STATUS: &str = "engine.status";
    /// Quit the app / daemon.
    pub const APP_QUIT: &str = "app.quit";
    /// All settings: `{"settings":{…}}`.
    pub const SETTINGS_GET: &str = "settings.get";
    /// Change settings. Args: a JSON merge patch; strict (ADR 0014).
    pub const SETTINGS_SET: &str = "settings.set";
    /// Games running now (detection) and the one being followed.
    pub const GAMES_LIST: &str = "games.list";
    /// Open the overlay's quick menu (what its hotkey does).
    pub const OVERLAY_MENU: &str = "overlay.menu";
    /// Clip folder size, cap, free space.
    pub const STORAGE_STATUS: &str = "storage.status";
    /// Bring the clip folder under its cap now.
    pub const STORAGE_CLEANUP: &str = "storage.cleanup";
    /// Star or unstar a clip. Args: `{"path": "…", "starred": true}`.
    pub const CLIP_STAR: &str = "clip.star";
    /// GPUs and working encoders, best first. Args: `{"codec": "h264"}` (optional).
    pub const ENCODERS_LIST: &str = "encoders.list";
    /// Audio inputs and outputs with their ids.
    pub const AUDIO_DEVICES: &str = "audio.devices";
    /// Microphone meter on/off (`mic_level` events). Args: `{"on": true, "device": null}`.
    pub const AUDIO_METER: &str = "audio.meter";
    /// Replay-buffer RAM estimate for (draft) settings. Args: settings JSON or null.
    pub const RECORDING_ESTIMATE: &str = "recording.estimate";
    /// Unbind hotkeys while a new one is typed (back after a minute at most).
    pub const HOTKEYS_PAUSE: &str = "hotkeys.pause";
    pub const HOTKEYS_RESUME: &str = "hotkeys.resume";
    /// Running programs with a window, to add as games.
    pub const GAMES_CANDIDATES: &str = "games.candidates";
    /// Show the overlay as if a clip had been saved (settings page, tests).
    pub const OVERLAY_PREVIEW: &str = "overlay.preview";
    /// Clips in the library. Args: `klip_library::Query` (all optional): `search`,
    /// `game`, `starred`, `tag`, `since_ms`, `sort`, `limit`.
    pub const LIBRARY_LIST: &str = "library.list";
    /// Make these clips' thumbnails next. Args: `{"keys": [key, …]}`.
    pub const LIBRARY_THUMBS: &str = "library.thumbs";
    /// The cached thumbnail file of a clip. Args: `{"key", "kind": "poster"|"strip"}`.
    pub const LIBRARY_THUMB: &str = "library.thumb";
    /// Rename a clip file. Args: `{"path", "name"}`.
    pub const CLIP_RENAME: &str = "clip.rename";
    /// Set a clip's tags. Args: `{"path", "tags": [..]}`.
    pub const CLIP_TAGS: &str = "clip.tags";
    /// Move clips to the Recycle Bin / Trash. Args: `{"paths": [..], "permanent": bool}`
    /// (`permanent`: delete instead).
    pub const CLIP_DELETE: &str = "clip.delete";
    /// Open a clip in the default player. Args: `{"path"}`.
    pub const CLIP_OPEN: &str = "clip.open";
    /// Show a clip in the file manager. Args: `{"path"}`.
    pub const CLIP_REVEAL: &str = "clip.reveal";
    /// `{path}` → the clip's editor project (new or saved), repair issues, `source`.
    pub const EDITOR_PROJECT_GET: &str = "editor.project.get";
    /// `{path, project}` → the project repaired and saved.
    pub const EDITOR_PROJECT_SET: &str = "editor.project.set";
    /// `{path}` → what the preview plays: `original` | `ready` (proxy path) | `making` (id).
    pub const EDITOR_PROXY: &str = "editor.proxy";
    /// `{path, layer, width, height}` → the text layer drawn at that output size:
    /// `{x, y, width, height, frame_width, frame_height, png}` (base64), the pixels the
    /// export composites.
    pub const EDITOR_TEXT_RENDER: &str = "editor.text.render";
    /// `{path, out_ms, width, project?}` → `{jpeg (base64), width, height, out_ms}`: the
    /// frame the export encodes at that time (the saved project, or `project`).
    pub const EDITOR_FRAME: &str = "editor.frame";
    /// `{path, add: [paths]}` → the project of `path` with the clips `add` appended on
    /// its montage track (a montage lives under its first clip).
    pub const EDITOR_MONTAGE: &str = "editor.montage";
    /// `{path}` → `{key, status: "ready", tracks: [{index, name, path}]}`: the clip's
    /// audio tracks copied one per file for the editor's preview (cached in
    /// `.klip/audio/<key>/`); `tracks` is empty for a clip without sound.
    pub const EDITOR_AUDIO: &str = "editor.audio";
    /// `{path}` → `{files: [{path, kind: image | audio, ok, error?}]}`: the files the
    /// clip's project places (stickers, sounds, music), checked: absolute, a picture or
    /// sound extension, there.
    pub const EDITOR_FILES: &str = "editor.files";
    /// `{path}` → `{snapshots: [{id, saved_unix_ms, bytes}]}`, newest first.
    pub const EDITOR_HISTORY_LIST: &str = "editor.history.list";
    /// `{path, id}` → the project as that snapshot saved it, saved again (the project it
    /// replaces becomes a snapshot).
    pub const EDITOR_HISTORY_RESTORE: &str = "editor.history.restore";
    /// `{path, preset}` (`preset: "custom"` with `custom: {width, height, fps,
    /// bitrate_kbps, codec}`) → `{id, out}`; `export_progress` / `export_done` follow.
    pub const EXPORT_START: &str = "export.start";
    /// `{id}` stops a running export.
    pub const EXPORT_CANCEL: &str = "export.cancel";
    /// → `{active, last}`.
    pub const EXPORT_STATUS: &str = "export.status";
    /// Custom themes (M10, ADR 0019): `{dir, themes, issues}`.
    pub const THEME_LIST: &str = "theme.list";
    /// Create or replace a custom theme from the editor (`{id?, theme, picture?}`).
    pub const THEME_SAVE: &str = "theme.save";
    /// Delete a custom theme (`{id}`); the theme in use falls back to Follow system.
    pub const THEME_DELETE: &str = "theme.delete";
    /// Import a `*.klip-theme` file (`{path}`) → `{id, theme, issues}`.
    pub const THEME_IMPORT: &str = "theme.import";
    /// Export a custom theme to one file (`{id, path}`).
    pub const THEME_EXPORT: &str = "theme.export";
}

/// Longest accepted line (bytes, including the newline).
pub const MAX_LINE: usize = 64 * 1024;

/// Authentication line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub auth: String,
}

/// A command request.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

/// Error body of a failed response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

/// A response to one request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

/// Error a command handler returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// Stable machine-readable code: `bad_args`, `unknown_command`, `not_ready`, `failed`,
    /// `internal`, `unauthorized`, `bad_request`.
    pub code: &'static str,
    pub message: String,
}

impl CommandError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        CommandError {
            code,
            message: message.into(),
        }
    }

    pub fn bad_args(message: impl Into<String>) -> Self {
        Self::new("bad_args", message)
    }

    pub fn unknown(cmd: &str) -> Self {
        // Echo at most 64 chars of a hostile command name.
        let shown: String = cmd.chars().take(64).collect();
        Self::new("unknown_command", format!("unknown command {shown:?}"))
    }
}

/// Executes commands. Implemented by the recorder (and later the app).
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, cmd: &str, args: &Value) -> Result<Value, CommandError>;
}

/// Contents of the discovery file the server writes for clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

/// Default discovery file: `%LOCALAPPDATA%\Klip\control.json` on Windows,
/// `$XDG_RUNTIME_DIR/klip/control.json` on Linux (falling back to the local data dir).
pub fn default_discovery_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        dirs::data_local_dir().map(|d| d.join("Klip").join("control.json"))
    }
    #[cfg(not(windows))]
    {
        dirs::runtime_dir()
            .map(|d| d.join("klip"))
            .or_else(|| dirs::data_local_dir().map(|d| d.join("klip")))
            .map(|d| d.join("control.json"))
    }
}

/// Read and validate a discovery file. Treats the file as hostile: size-capped, strict
/// schema.
pub fn read_discovery(path: &Path) -> Result<Discovery, ClientError> {
    let meta = std::fs::metadata(path).map_err(|_| ClientError::NotRunning)?;
    if meta.len() > 4096 {
        return Err(ClientError::Protocol("discovery file too large".into()));
    }
    let text = std::fs::read_to_string(path).map_err(|_| ClientError::NotRunning)?;
    let d: Discovery = serde_json::from_str(&text)
        .map_err(|e| ClientError::Protocol(format!("discovery file: {e}")))?;
    if d.port == 0 || d.token.len() != 64 || !d.token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ClientError::Protocol(
            "discovery file has invalid values".into(),
        ));
    }
    Ok(d)
}

#[cfg(test)]
mod tests;
