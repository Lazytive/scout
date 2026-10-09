# klip-engine

Klip's engine (ADR 0014): everything the app does that isn't UI. The desktop app and
`klip-cli daemon` are front ends to it.

- `games`: game detection from a `Snapshot` (processes, visible windows, executable paths
  of processes with a big enough window, the foreground window). A program is a game
  when it is on the user's list (window ≥ 320x240), or when its path is inside a store's
  library folder (Steam `steamapps/common`, Epic Games, GOG Galaxy/GOG Games, XboxGames,
  EA/Origin Games, Ubisoft Game Launcher/games, Riot Games, itch/apps, Heroic) and it has
  a window ≥ 640x480. The folder after the library is the game's name (store
  sub-folders like Steamworks Shared, SteamVR or Riot Client are not games). Store
  clients, browsers, Discord, OBS and helpers (crash handlers/reporters, launchers,
  installers, updaters, anti-cheat services, overlays) are left out unless the user
  listed them; the user's "never" list always wins. Results: foreground first, then by
  window size. Without a window list (Wayland) every process is checked by path, and the
  path's file name is used (Proton: the Windows program, not `wine64-preloader`).
  Only OS process and window lists and `PROCESS_QUERY_LIMITED_INFORMATION` paths are
  read: nothing touches a game process (rule 1).
  `folder_name` makes a safe per-game folder name; `name_from_exe` strips `.exe` and
  Unreal's `-Win64-Shipping`.
  Measured (Windows dev machine, ~300 processes): one poll (snapshot + detect) median
  3.6 ms, max 4.7 ms; at one poll per 2 s that is ≈ 0.2 % of one core.

- `Engine` (`engine.rs`): `Engine::start(EngineOptions)` loads the settings (repaired
  files are rewritten, the original kept as `.bad`), warms up the encoders, binds the
  hotkeys, opens the control channel (if on) and starts the `klip-watch` thread. Every
  poll (2 s): buffer mode `off` → nothing runs; a followed game that exited → its
  recorder stops (a running recording is finalised); otherwise the best detected game is
  followed and its recorder started (`config::plan`: profile applied, window target,
  monitor refresh for High, encoders from the probe cache, clips in
  `<clip folder>/<Game>/`); mode `always` records the primary monitor (`Desktop`) when no
  game runs and makes way when one starts. A recorder that fails to start is retried
  after 30 s. Leftover recordings are finalised at start (recordings folder and its game
  folders). Hotkeys come from the settings (ids `clip.save.0`, `session.toggle.0`, …);
  a press runs its action on a job thread (one at a time per hotkey, under
  `catch_unwind`); changed hotkeys are rebound at once, other changes apply when the next
  game starts. Commands: `ping`, `engine.status`, `settings.get`, `settings.set` (strict
  merge patch, saved atomically), `games.list`, `clip.save`, `session.*`, `bookmark.add`,
  `recorder.status`, `overlay.menu`, `recorder.stop`/`app.quit` (the front end exits).
- Storage (`engine/storage.rs`): after every saved clip (that clip protected), at start
  and when the storage or folder settings change, a `klip-storage` job scans the clip
  folder and removes the oldest unstarred clips over the cap (`klip_library::over_cap`):
  to the Recycle Bin / Trash by default, deleted with `over_cap: delete`; the event
  `storage_cleaned` reports what went and what failed. Free space on the clip drive is
  checked at recorder start and after clean-ups: below `low_disk_gb` → a `warning`
  event (at most every 30 min). Commands `storage.status`, `storage.cleanup`,
  `clip.star`.
- Setup commands (`engine/setup.rs`, for the settings page and first run):
  `encoders.list` (GPUs + working encoders best first, from the probe cache, probing if
  needed), `audio.devices`, `audio.meter` (opens an input and emits `mic_level` peak/RMS
  dBFS ~15/s; stops by itself after 2 min), `recording.estimate` (buffer RAM for draft
  settings: preset at the primary monitor's refresh, audio tracks as the recorder lays
  them out), `hotkeys.pause` / `hotkeys.resume` (unbind while a hotkey field listens;
  back by themselves after 1 min), `games.candidates` (programs with a window to add
  as games; `games::candidates`, no window titles).
- `events`: `Event` (`ready`, `game_started/stopped`, `recorder_started/stopped/failed`,
  `clip_saved` with the game's window, `recording_started/stopped`, `bookmark`,
  `action_failed` for hotkeys, `quick_menu`, `mic_level`, `hotkeys` with each binding's error,
  `settings_changed`, `storage_cleaned`, `recovered`, `warning`), fanned out to bounded queues (256 per
  subscriber; a stalled subscriber loses events, memory doesn't grow).
- `config`: settings → `RecorderConfig`, shared with `klip-cli record`: clip and
  recordings folders, encoder choice + probe cache, target GPU and refresh rate, audio
  inputs (Game = the game's process tree, or System without music players; Voice =
  running voice apps; Microphone), screen-cast portal with restore token (Linux).

Tests: store folders on Windows, Linux and Proton paths (incl. non-ASCII), helpers vs
games (`CrashBandicoot.exe` is a game), safe folder names (reserved device names,
illegal characters, length), detection order (foreground, window size), user and never
lists, minimised windows, window-less detection, and the poll cost on the real system.
`tests/engine.rs` (no recording: buffer off, control channel off): commands, `not_ready`
without a game, strict settings changes (nothing changes on disk or in memory when
rejected), `settings_changed`, hotkeys rebound from settings, a broken settings file
repaired and rewritten, the storage cap (1.3 GiB of test clips against 1 GiB: the oldest
unstarred goes at start, the starred older one stays; usage, low-disk warning,
`clip.star` checks), the setup commands (estimate scales with the buffer and preset,
hotkeys pause and resume, candidates, devices, meter arguments; `mic_meter_reports_levels`
is a hardware test on the default microphone). End to end with the test game: `apps/klip-cli/tests/m6_daemon.rs`.

Layer: media engine (no UI).
