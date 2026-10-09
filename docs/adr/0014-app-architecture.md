# 0014: Desktop app architecture: engine crate, settings file, thin Tauri shell

Status: accepted (M6, 2026-10-09). Written at M6.1, completed with what M6 built.

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
- **Game detection** (M6.3): a program is a game when the user listed it, or when its
  executable is inside a store's library folder (Steam, Epic, GOG, Xbox, EA, Ubisoft,
  Riot, itch, Heroic) and it has a window of at least 640x480; store clients, browsers,
  launchers and helpers are left out, the user's "never" list always wins. Only OS
  process/window lists and `PROCESS_QUERY_LIMITED_INFORMATION` paths are read. One poll
  every 2 s (3.6 ms). The engine follows the game's *process*: the recorder starts when
  a game is found and stops when the process exits; replay buffer modes `games`
  (default), `always` (the desktop when no game runs) and `off`.
- **Storage** (M6.5): stars live in `<clip folder>/.klip/library.json` (losing it loses
  stars, never clips); over the size cap (GiB) the oldest unstarred clips go to the
  Recycle Bin / Trash (default) or are deleted, never the clip just saved.
- **Tauri shell** (M6.6): one Tauri command, `klip(cmd, args)`, forwards to the engine's
  command handler (the same as the control channel's), and engine events go to the
  window as `klip://event`; the UI has no other back door. Single instance; closing the
  window keeps Klip in the tray; quitting shuts the engine down first. The tray icon is
  drawn at runtime (no asset). Test overrides by environment (`KLIP_SETTINGS`,
  `KLIP_DISCOVERY`, `KLIP_WINDOW_POS`) keep test runs away from the user's settings,
  hotkeys and focus; UI checks run the window off-screen and capture it with
  `PrintWindow` (`tools/window-shot.ps1`). The built front end is embedded by default
  (`custom-protocol` feature).
- **Look and themes** (M6.6–M6.7, ADR 0004): the maintainer's Photocraft / FilmCraft
  style. Themes are colour tokens only: follow system (default, the maintainer's
  choice), Dark (FilmCraft greys), Graphite (Photocraft greys), Light, OLED black; every
  text and outline colour pair is tested for WCAG AA contrast.
- **Settings UI and first run** (M6.8): every control saves at once through the strict
  `settings.set` and shows the engine's rejection under the field. While a hotkey field
  listens, the engine's hotkeys are unbound (`hotkeys.pause`, back by themselves after
  a minute) so the keys reach the field. Start with the OS is applied only when the
  setting changes, so a choice made in the OS's startup-apps list isn't overridden.
- **Overlay** (M6.9): a separate transparent, click-through window that is never
  activated (`WS_EX_NOACTIVATE`, shown with `SWP_NOACTIVATE`), on the game's monitor.
  On by default as the Minimal card (the maintainer's choice); Full adds a recording dot
  and a quick menu (the only state that takes focus, which it hands back to the game);
  Off or a game in exclusive fullscreen: no window, only the sound (synthesised, no
  asset) and/or a system notification. Thumbnails are decoded from the saved clip after
  the save (the card appears at once, the picture follows) and cached for the library.
- **Performance** (M6.10): measured with `cargo xtask bench` by switching the replay
  buffer off and on within one long run of the benchmark scene (separate runs drift
  more than the effect). Capture asks Windows for at most twice the recording rate
  (`MinUpdateInterval`); skipping frames in Klip was tried and dropped (it left slots
  empty).

## Consequences
- Settings, game rules and storage policy are unit- and property-tested in pure crates;
  the engine is tested end to end through `klip-cli daemon` with the test game; the
  Tauri layer needs only smoke tests and screenshots.
- Two front doors (CLI daemon, app) run one engine, so a fix lands in both.
- `RegisterHotKey` takes the keys away from the focused game; users who need the same
  keys in a game must pick another combination (the settings page says so).
- Measured cost on the benchmark scene (RTX 4080 SUPER): none with a 144 fps cap,
  2.9 % ± 1.0 % fewer frames uncapped; the engine uses 6 % of one core. But once a game
  was recorded the process keeps ~380 MB private memory (encoder, decoder and driver
  state): moving capture/encode and thumbnails into a child process is the likely fix.
- The overlay is Windows-only so far; on Linux the sound and notification stand in
  (layer-shell later). The Linux engine paths are written but not yet run.
- Whether a window over a borderless game turns VRR off while the card shows is not
  measured (human checklist; PresentMon could measure it).
