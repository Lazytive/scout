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

Tests: store folders on Windows, Linux and Proton paths (incl. non-ASCII), helpers vs
games (`CrashBandicoot.exe` is a game), safe folder names (reserved device names,
illegal characters, length), detection order (foreground, window size), user and never
lists, minimised windows, window-less detection, and the poll cost on the real system.

Layer: media engine (no UI).
