# 0014: Desktop app architecture: engine crate, settings file, thin Tauri shell

Status: proposed (M6.1, 2026-10-09); confirmed at the end of M6.

## Context
M6 turns the recorder CLI into a tray app: settings, first-run setup, hotkeys, game
detection with per-game profiles, quality presets, storage management, an overlay and
themes. Most of this is logic that must be tested without a webview (hostile settings
files, which clips to delete, which process is a game), and the CLI, the app, `xtask
bench` and the e2e tests should all run the same code. Tauri builds are slow and its
windows are hard to drive from tests.

## Decision
- **Layers** (ADR 0001 extended):
  - `klip-settings` (pure): the settings schema, quality presets, per-game profiles and
    the hotkey syntax. No OS calls besides reading/writing the settings file.
  - `klip-library` (pure, plus the `trash` crate): metadata of the clip folder (stars)
    and the storage-cap policy.
  - `klip-engine` (media engine layer, no UI): owns the settings, watches for games,
    starts and stops the `Recorder`, binds hotkeys, emits events and implements every
    command (`klip_control::Handler`). The CLI's config builders (encoder choice + cache,
    audio plan, capture target) move here so the CLI and the app share them.
    `klip-cli daemon` runs the engine headless (e2e tests, bench, people without a
    desktop session).
  - `apps/klip-app` (Tauri 2 + Svelte 5, ADR 0004): tray, windows, overlay, front end.
    It forwards UI actions to the engine's command handler and engine events to the UI;
    it holds no recording logic.
- **Settings file**: JSON at `<config dir>/Klip/settings.json` (`%APPDATA%\Klip` on
  Windows, `~/.config/Klip` on Linux), schema `version` 1, written atomically (temp file
  + rename).
  - Loading never fails. Invalid JSON, a non-object or a file over 1 MiB → defaults, and
    the file is kept as `settings.json.bad`. A field with a wrong type or unknown enum
    value is dropped (one at a time, located with `serde_path_to_error`) and gets its
    default; a bad list element is dropped; then values are clamped to their ranges,
    lists trimmed, deduplicated and capped. Every repair is reported as an issue
    (path + message) for the log and the UI. Unknown keys are ignored (a newer Klip's
    file still loads).
  - Changing settings (`settings.set`) takes a JSON merge patch (RFC 7396) and is
    strict: unknown keys, wrong types and out-of-range values are rejected with the
    path, nothing is changed.
  - Per-game profiles are keyed by the lower-case executable name and override buffer
    length, quality and audio sources; `Settings::effective(game)` merges them.
- **Quality presets** resolve to numbers at recording start: Low 720p30 8 Mbit/s,
  Balanced 1080p60 20 Mbit/s (default), High up to 1440p at the monitor's refresh
  clamped to 60–144 fps, 40 Mbit/s, Custom (size, fps, bitrate, H.264/HEVC/AV1).
  Presets use H.264 (plays everywhere). The numbers are checked by `xtask bench` (M6.10).
- **Hotkeys**: stored as `Ctrl+Alt+Shift+Win+Key` strings, converted to the shortcuts-spec
  trigger format (`CTRL+ALT+SHIFT+LOGO+keysym`) for `klip_platform::GlobalShortcuts`.
  Windows binds them with `RegisterHotKey` (works while a game has focus, no hooks into
  any process, so nothing an anti-cheat can object to; the key combination is consumed,
  so the defaults use Alt + F-keys); Linux uses the GlobalShortcuts portal (M4). A
  combination another app already holds is reported, not silently ignored. Defaults:
  Alt+F10 clip 30 s, Alt+Shift+F10 clip 2 min, Alt+F9 start/stop recording, Alt+F8
  bookmark, quick menu unbound.
- The control channel (ADR 0006) is hosted by the engine and on by default (loopback,
  token file readable only by the user), so `klip-cli clip` works as a desktop-shortcut
  fallback; a setting turns it off.

## Consequences
- Settings, game rules and storage policy are unit- and property-tested in pure crates;
  the engine is tested end to end through `klip-cli daemon` with the test game; the
  Tauri layer needs only smoke tests and screenshots.
- Two front doors (CLI daemon, app) run one engine, so a fix lands in both.
- `RegisterHotKey` takes the keys away from the focused game; users who need the same
  keys in a game must pick another combination (the settings page says so).
