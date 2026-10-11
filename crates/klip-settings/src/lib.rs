//! Klip's settings (ADR 0014): the schema with defaults, loading that never fails on a
//! hostile or broken file, strict changes through JSON merge patches, quality presets,
//! per-game profiles and the hotkey syntax.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod file;
pub mod hotkey;
mod lenient;
pub mod preset;
pub mod theme_file;
mod validate;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use file::{LoadStatus, Loaded, load, save};
pub use hotkey::{Hotkey, HotkeyError, Key};
pub use preset::{CustomQuality, Preset, Quality, QualityParams, VideoCodec};

/// Version of the settings schema written by this build.
pub const SCHEMA_VERSION: u32 = 1;
/// Settings files bigger than this are not read.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Most entries kept in any app or game list.
pub const MAX_LIST: usize = 256;
/// Most per-game profiles.
pub const MAX_PROFILES: usize = 1024;
/// Most hotkey bindings.
pub const MAX_HOTKEYS: usize = 32;
/// Longest executable or game name.
pub const MAX_NAME: usize = 260;
/// Longest folder path or device id.
pub const MAX_PATH: usize = 4096;

/// Replay buffer length, seconds.
pub const BUFFER_SECS: std::ops::RangeInclusive<u32> = 5..=1800;
/// Clip length a hotkey saves, seconds.
pub const CLIP_SECS: std::ops::RangeInclusive<u32> = 5..=3600;
/// AAC bitrates per track the encoders accept, kbit/s.
pub const AUDIO_BITRATES: &[u32] = &[96, 128, 160, 192, 256, 320];
/// Overlay pop-up time, seconds.
pub const OVERLAY_SECS: std::ops::RangeInclusive<u32> = 1..=10;
/// Storage cap, GB (0 = no cap).
pub const CAP_GB_MAX: u32 = 1_000_000;
/// Low-disk warning threshold, GB (0 = off).
pub const LOW_DISK_GB_MAX: u32 = 100_000;
/// Discord export size limit, MB (decimal, as Discord counts).
pub const DISCORD_LIMIT_MB: std::ops::RangeInclusive<u32> = 1..=1000;

/// Something in a settings file or patch that was wrong (and, when loading, repaired).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// Where, e.g. `recording.buffer_secs` or `hotkeys[2].keys` (empty: the whole file).
    pub path: String,
    pub message: String,
}

impl Issue {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Issue {
        Issue {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Errors from changing settings.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The change was rejected; nothing was changed.
    #[error("{0}")]
    Invalid(Issue),
    #[error("settings could not be serialised: {0}")]
    Serialize(String),
}

/// All settings. Every struct falls back to its defaults for missing fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub app: AppSettings,
    pub clips: ClipSettings,
    pub recording: RecordingSettings,
    pub audio: AudioSettings,
    pub hotkeys: Vec<HotkeyBinding>,
    pub overlay: OverlaySettings,
    pub storage: StorageSettings,
    pub appearance: AppearanceSettings,
    pub games: GameSettings,
    pub export: ExportSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SCHEMA_VERSION,
            app: AppSettings::default(),
            clips: ClipSettings::default(),
            recording: RecordingSettings::default(),
            audio: AudioSettings::default(),
            hotkeys: default_hotkeys(),
            overlay: OverlaySettings::default(),
            storage: StorageSettings::default(),
            appearance: AppearanceSettings::default(),
            games: GameSettings::default(),
            export: ExportSettings::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// The first-run setup was finished (or skipped).
    pub first_run_done: bool,
    /// Start with the OS (tray only).
    pub autostart: bool,
    /// Open to the tray instead of showing the window.
    pub start_minimized: bool,
    /// Closing the window keeps Klip running in the tray.
    pub close_to_tray: bool,
    /// Local control channel for `klip-cli` and scripts (loopback + token, ADR 0006).
    pub control_channel: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            first_run_done: false,
            autostart: false,
            start_minimized: true,
            close_to_tray: true,
            control_channel: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipSettings {
    /// Absolute folder for clips; `None`: `Videos/Klip`.
    pub folder: Option<String>,
    /// Clips go into a sub-folder per game.
    pub per_game_folders: bool,
    /// Absolute folder for full-session recordings; `None`: the clip folder.
    pub recordings_folder: Option<String>,
}

impl Default for ClipSettings {
    fn default() -> Self {
        ClipSettings {
            folder: None,
            per_game_folders: true,
            recordings_folder: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSettings {
    /// Replay buffer length in seconds ([`BUFFER_SECS`]).
    pub buffer_secs: u32,
    pub quality: Quality,
    pub show_cursor: bool,
    /// Encoder element to use instead of the automatic choice (advanced).
    pub encoder: Option<String>,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        RecordingSettings {
            buffer_secs: 120,
            quality: Quality::default(),
            show_cursor: true,
            encoder: None,
        }
    }
}

/// Which desktop audio goes on the first track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopAudio {
    /// Only the game (per-process capture; all system audio where that's unsupported).
    #[default]
    Game,
    /// Everything except the excluded apps.
    System,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub desktop: DesktopAudio,
    /// Voice-chat apps on their own track.
    pub voice: bool,
    /// Executables recorded on the voice track (when running).
    pub voice_apps: Vec<String>,
    pub mic: bool,
    /// Microphone device id; `None`: the default input.
    pub mic_device: Option<String>,
    /// Left out of system audio, besides the music players when `default_excludes`.
    pub exclude_apps: Vec<String>,
    /// Leave known music players out of system audio.
    pub default_excludes: bool,
    /// Each source on its own track (plus the mix first); otherwise only the mix.
    pub separate_tracks: bool,
    /// AAC bitrate per track, kbit/s ([`AUDIO_BITRATES`]).
    pub bitrate_kbps: u32,
}

/// Voice-chat apps recorded on the voice track by default.
pub fn default_voice_apps() -> Vec<String> {
    let names: &[&str] = if cfg!(windows) {
        &[
            "Discord.exe",
            "DiscordPTB.exe",
            "DiscordCanary.exe",
            "TeamSpeak.exe",
            "ts3client_win64.exe",
            "mumble.exe",
        ]
    } else {
        &["Discord", "discord", "vesktop", "teamspeak", "mumble"]
    };
    names.iter().map(ToString::to_string).collect()
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings {
            desktop: DesktopAudio::Game,
            voice: true,
            voice_apps: default_voice_apps(),
            mic: false,
            mic_device: None,
            exclude_apps: Vec::new(),
            default_excludes: true,
            separate_tracks: true,
            bitrate_kbps: 160,
        }
    }
}

/// What a hotkey does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotkeyAction {
    /// Save the last `secs` seconds.
    #[default]
    #[serde(rename = "clip.save")]
    ClipSave,
    /// Start or stop a full-session recording.
    #[serde(rename = "session.toggle")]
    SessionToggle,
    #[serde(rename = "bookmark.add")]
    Bookmark,
    /// Open the overlay's quick menu (Full overlay).
    #[serde(rename = "overlay.menu")]
    QuickMenu,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    /// `Ctrl+Alt+Shift+Win+Key`; empty: not bound.
    pub keys: String,
    /// Clip length for `clip.save` ([`CLIP_SECS`]); 0 for other actions.
    pub secs: u32,
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        HotkeyBinding {
            action: HotkeyAction::ClipSave,
            keys: String::new(),
            secs: 30,
        }
    }
}

impl HotkeyBinding {
    /// The parsed keys (`None` when unbound or invalid).
    pub fn hotkey(&self) -> Option<Hotkey> {
        self.keys.parse().ok()
    }
}

/// Default bindings (ADR 0014).
pub fn default_hotkeys() -> Vec<HotkeyBinding> {
    let b = |action, keys: &str, secs| HotkeyBinding {
        action,
        keys: keys.into(),
        secs,
    };
    vec![
        b(HotkeyAction::ClipSave, "Alt+F10", 30),
        b(HotkeyAction::ClipSave, "Alt+Shift+F10", 120),
        b(HotkeyAction::SessionToggle, "Alt+F9", 0),
        b(HotkeyAction::Bookmark, "Alt+F8", 0),
        b(HotkeyAction::QuickMenu, "", 0),
    ]
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayMode {
    /// No window: sound cue and/or system notification.
    Off,
    /// A small "Clip saved" pop-up with a thumbnail.
    #[default]
    Minimal,
    /// Pop-up + recording dot + quick menu.
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    pub mode: OverlayMode,
    /// Play a short sound when a clip is saved.
    pub sound: bool,
    /// System notification when a clip is saved (also used when no pop-up can show).
    pub notification: bool,
    pub corner: Corner,
    /// How long the pop-up stays ([`OVERLAY_SECS`]).
    pub seconds: u32,
    /// Full mode: a dot while a full-session recording runs.
    pub recording_dot: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        OverlaySettings {
            mode: OverlayMode::Minimal,
            sound: true,
            notification: false,
            corner: Corner::TopRight,
            seconds: 3,
            recording_dot: true,
        }
    }
}

/// What happens to the oldest unstarred clips when the folder is over the cap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverCap {
    /// Recycle Bin / Trash.
    #[default]
    Trash,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageSettings {
    /// Size cap for the clip folder in GB (0: no cap).
    pub cap_gb: u32,
    pub over_cap: OverCap,
    /// Warn when the clip drive has less free space than this, GB (0: never).
    pub low_disk_gb: u32,
}

impl Default for StorageSettings {
    fn default() -> Self {
        StorageSettings {
            cap_gb: 50,
            over_cap: OverCap::Trash,
            low_disk_gb: 10,
        }
    }
}

/// The editor's export (ADR 0017).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    /// The Discord preset keeps files under this many MB (10 without Nitro).
    pub discord_limit_mb: u32,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            discord_limit_mb: 10,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the OS light/dark setting (Light or Dark).
    #[default]
    System,
    /// Near-black panels (like FilmCraft).
    Dark,
    /// Mid-grey panels (like Photocraft).
    Graphite,
    Light,
    /// Pure black backgrounds.
    Oled,
    /// The custom theme `appearance.custom_theme` (M10).
    Custom,
}

/// Spacing and control sizes (M10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    /// The standard look.
    #[default]
    Comfortable,
    /// Tighter spacing and smaller controls.
    Compact,
}

/// UI text size (M10): 90 %, 100 %, 115 %, 130 %.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSize {
    Small,
    #[default]
    Default,
    Large,
    Larger,
}

/// Animations (M10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    /// Follow the OS "reduce motion" setting.
    #[default]
    System,
    /// No animations or transitions.
    Reduce,
    /// Always animate.
    Full,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: Theme,
    /// Id of the custom theme (a file in `<settings dir>/themes/`), used when `theme` is
    /// `custom`; kept when another theme is picked.
    pub custom_theme: Option<String>,
    /// Accent colour `#rrggbb`; `None`: the theme's own.
    pub accent: Option<String>,
    pub density: Density,
    pub text_size: TextSize,
    pub motion: Motion,
}

/// When the replay buffer runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BufferMode {
    /// While a detected game runs (near-zero cost otherwise).
    #[default]
    Games,
    /// Always, capturing the primary monitor when no game runs.
    Always,
    /// Never (hotkeys and recordings do nothing).
    Off,
}

/// A program the user marked as a game.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserGame {
    /// Executable file name, e.g. `MyGame.exe`.
    pub exe: String,
    /// Display name; `None`: derived from the folder or the executable.
    pub name: Option<String>,
}

/// Per-game overrides; `None` keeps the global setting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub buffer_secs: Option<u32>,
    pub quality: Option<Quality>,
    pub desktop_audio: Option<DesktopAudio>,
    pub voice: Option<bool>,
    pub mic: Option<bool>,
    /// Automatic clips from official game integrations (M12).
    pub auto_clip: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    pub buffer: BufferMode,
    /// Games found in store library folders (Steam, Epic, …) count; off: only
    /// `user_games` (tests set this so a game the human plays is never followed).
    pub detect_stores: bool,
    pub user_games: Vec<UserGame>,
    /// Executables never treated as games.
    pub never: Vec<String>,
    /// Keyed by [`game_key`].
    pub profiles: BTreeMap<String, Profile>,
}

impl Default for GameSettings {
    fn default() -> Self {
        GameSettings {
            buffer: BufferMode::default(),
            detect_stores: true,
            user_games: Vec::new(),
            never: Vec::new(),
            profiles: BTreeMap::new(),
        }
    }
}

/// Profile key for an executable: its lower-case file name.
pub fn game_key(exe: &str) -> String {
    exe.trim().to_lowercase()
}

/// Settings for one recording, with the game's profile applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub buffer_secs: u32,
    pub quality: Quality,
    pub show_cursor: bool,
    pub encoder: Option<String>,
    pub audio: AudioSettings,
    pub auto_clip: bool,
}

impl Settings {
    /// The settings a recording of `game` (an executable name) uses.
    pub fn effective(&self, game: Option<&str>) -> Effective {
        let profile = game.and_then(|g| self.games.profiles.get(&game_key(g)));
        let mut audio = self.audio.clone();
        if let Some(p) = profile {
            if let Some(d) = p.desktop_audio {
                audio.desktop = d;
            }
            if let Some(v) = p.voice {
                audio.voice = v;
            }
            if let Some(m) = p.mic {
                audio.mic = m;
            }
        }
        Effective {
            buffer_secs: profile
                .and_then(|p| p.buffer_secs)
                .unwrap_or(self.recording.buffer_secs),
            quality: profile
                .and_then(|p| p.quality)
                .unwrap_or(self.recording.quality),
            show_cursor: self.recording.show_cursor,
            encoder: self.recording.encoder.clone(),
            audio,
            auto_clip: profile.and_then(|p| p.auto_clip).unwrap_or(false),
        }
    }

    /// As a JSON value (what `settings.get` returns).
    pub fn to_value(&self) -> Result<Value, SettingsError> {
        serde_json::to_value(self).map_err(|e| SettingsError::Serialize(e.to_string()))
    }

    /// Settings from a JSON text, repairing whatever is wrong (never fails). The issues
    /// say what was repaired.
    pub fn from_json_lenient(text: &str) -> (Settings, Vec<Issue>) {
        lenient::from_json(text)
    }

    /// Apply a JSON merge patch (RFC 7396) strictly: unknown keys, wrong types and
    /// out-of-range values are rejected and nothing changes. `version` can't be patched.
    pub fn apply_patch(&self, patch: &Value) -> Result<Settings, SettingsError> {
        lenient::apply_patch(self, patch)
    }

    /// Clamp values into range, normalise names and keys, drop bad entries. Returns what
    /// was wrong (normalising alone, like re-spelling a hotkey, is not an issue).
    pub fn validate(&mut self) -> Vec<Issue> {
        let mut issues = Vec::new();
        validate::settings(self, &mut issues);
        issues
    }
}

#[cfg(test)]
mod tests;
