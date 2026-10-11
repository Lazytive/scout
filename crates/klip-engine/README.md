# klip-engine

Scout's engine (ADR 0014): everything the app does that isn't UI. The desktop app and
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

- `migrate` (ADR 0020): Klip → Scout at the first start of Scout: settings and custom
  themes copied from `<config>/Klip`; the default `Videos/Klip` moved to `Videos/Scout` by
  one directory rename, or kept (written into the settings) when it can't move; a chosen
  folder is never touched. `NAME` / `LEGACY_NAME` name the folders. Run by the app and
  `klip-cli daemon`, never with a custom settings file.
- Custom themes (`engine/themes.rs`, ADR 0019): `theme.list|save|delete|import|export`
  on `<settings dir>/themes/`.
- `Engine` (`engine.rs`): `Engine::start(EngineOptions)` loads the settings (repaired
  files are rewritten, the original kept as `.bad`), warms up the encoders, binds the
  hotkeys, opens the control channel (if on) and starts the `klip-watch` thread. Every
  poll (2 s): buffer mode `off` → nothing runs; a followed game that exited → its
  recorder stops (a running recording is finalised); otherwise the best detected game is
  followed and its recorder started (encoders from the probe cache via
  `config::encoders_for`, then `config::plan_with`: profile applied, window target,
  monitor refresh for High, clips in `<clip folder>/<Game>/`); mode `always` records the
  primary monitor (`Desktop`) when no game runs and makes way when one starts. A
  recorder that fails to start is retried after 30 s. Leftover recordings are finalised
  at start (recordings folder and its game folders). Hotkeys come from the settings
  (ids `clip.save.0`, `session.toggle.0`, …); a press runs its action on a job thread
  (one at a time per hotkey, under `catch_unwind`); changed hotkeys are rebound at once,
  other changes apply when the next game starts. Commands: `ping`, `engine.status`,
  `settings.get`, `settings.set` (strict merge patch, saved atomically), `games.list`,
  `clip.save`, `session.*`, `bookmark.add`, `recorder.status`, `overlay.menu`,
  `recorder.stop`/`app.quit` (the front end exits).
- Recorder process (ADR 0016, `engine/host.rs` + `worker.rs`): with `Isolation::Child`
  (the default when the executable calls `klip_engine::worker_main()`, as `klip-cli` and
  `klip-app` do) each recorder runs in `<exe> --klip-recorder`, driven over its
  stdin/stdout (start request with settings, game and encoder names; then
  `klip_control` requests; `worker.status`, `worker.finish`), so the encoder and driver
  memory goes away when the game exits. Stopping waits for the process to exit. A
  recorder process that dies is reported (`recorder_stopped`, "ended unexpectedly"),
  restarted at once the first time per game (then after 30 s), and its recording is
  finalised by recovery 16 s later. `engine.status` has `recorder_pid`.
  `Isolation::InProcess` (unit tests, tools) keeps the recorder in the engine.
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
- Overlay support (`engine/overlay.rs`): after each saved clip a `klip-thumbnail` job
  takes a 320 px JPEG near its end (`klip_capture::thumbnail`, through a one-shot media
  worker process unless `media_worker` is `None`), caches it as the clip's
  library poster (`<clips>/.klip/thumbs/<key>.jpg`, `klip_library::thumb_path`), emits
  `thumbnail` with a data URL and `thumbs_ready`, and queues the clip's strip;
  `overlay.preview` emits `overlay_preview`.
- Library (`engine/library.rs`, ADR 0015): `library.list` (a `klip_library::Query`;
  scans the folder, probes new/changed clips into the index, prunes orphaned thumbnails
  once per folder, returns the listing with counts, strip layout and how many thumbnails
  are missing), `library.thumbs` (make these keys' thumbnails next), `library.thumb`
  (the cached file, for the app's URI scheme), `clip.rename`, `clip.tags`, `clip.delete`
  (Recycle Bin / Trash; `permanent: true` deletes; never a recording in progress),
  `clip.open`, `clip.reveal`; `library_changed` after rename / tags / star / delete. A
  `klip-thumbs` thread makes missing posters and strips through the media worker:
  requested keys first, then the rest newest first, but only while no game is followed
  (the game stopping wakes it); failures are remembered in the index and emitted as
  `thumbs_ready` with `failed`.
- Editor (`engine/editor.rs`, M8.4, M9.4): `editor.project.get {path}` → the clip's
  project (`.klip/projects/<key>.json` through `klip_edit::load`, or a new one), its
  repair issues, `status` (`new` / `saved` / `repaired` / `reset`), the clip's
  `SourceInfo` (from the index, else a probe; the clip's length always wins over the
  project's; titled chapters as `events`) and `clips`: the other library clips the
  project places (a montage lives under its first clip) as `{key, name, status: ok |
  missing | unreadable, source}`. A missing clip is reported, never dropped; one that
  runs past its file's end is shortened (`Advanced::fit_library_clips`).
  `editor.project.set {path, project}` → repaired (unknown keys kept) and saved
  atomically (≤ 1 MiB; an unchanged project isn't rewritten). The project a save
  replaces is kept as a snapshot in `.klip/history/<key>/<unix ms>.json` when the newest
  is older than a minute (30 kept): `editor.history.list {path}` → `{snapshots: [{id,
  saved_unix_ms, bytes}]}` newest first; `editor.history.restore {path, id}` saves the
  snapshot as the project (the replaced one becomes a snapshot, so it can be undone).
  `editor.montage {path, add: [paths]}` appends the clips, whole, after the end of the
  edit on the first video track (`Advanced::append_clips`; a "Montage" track when there
  is none). `editor.frame {path, out_ms, width, project?}` → `{jpeg (base64), width,
  height, out_ms}`: `klip_export::frame` of the saved (or the given) project in a media
  worker of its own (`Inner::frames`, stopped by the engine tick after 20 s idle; the
  JPEG goes through a temp file, the reply line is capped); `missing` when a clip on
  screen at that time is gone. A renamed clip is followed by every montage that places
  it (`retarget_montages`). `editor.proxy {path}` → what the preview plays: `original`
  for an H.264 clip, else the H.264 proxy in `.klip/proxies/<key>.mp4` (`ready`, or
  `making` with the id of the export job that makes it; `preset: "proxy"`).
  `editor.audio {path}` (M9.5) → `{key, status: "ready", tracks: [{index, name, path}]}`:
  the clip's audio tracks copied one per file (`klip_export::audio::split_audio`,
  `AudioJob` in the frame worker, timeout 30 s + 20 s per GB) into
  `.klip/audio/<key>/<n>.m4a` with a `tracks.json` stamp (size, modification time,
  count): remade when the clip changes, follows rename/delete/prune like the other
  caches. Names come from the file (`klip_library::audio_track_names`); the preview's
  mixer weighs them like the export (`mixer::volume_for`). `editor.files {path}` →
  `{files: [{path, kind: image | audio, ok, error?}]}`: the saved project's sticker,
  sound and music files, `ok` only for a full path with a picture/sound extension that
  exists (the app allows only those for the asset protocol).
- Export (`engine/export.rs`, M8.4, M9.4): `export.start {path, preset}` (`original` /
  `youtube` / `tiktok` / `discord`, or `custom` with `custom: {width, height, fps,
  bitrate_kbps, codec: h264 | hevc | av1}`, every number range-checked; the file is
  `<name> - Custom.mp4`) renders the saved project (a montage with its clips' chapters
  as markers; `missing` naming the clips that are gone) with `klip-export` on a
  `klip-export-<id>` thread through its own media worker into
  `<name> - <Preset>.mp4` next to the clip (`Library::unique_sibling`: ` (2)`, ` (3)`
  … when taken; never overwrites) → `{id, out}`; one at a time (`busy`), while the
  preview proxy has a slot of its own (one at a time too) so neither blocks the other;
  `export.cancel {id}` (either kind; `not_found` otherwise); `export.status` →
  `{active, proxy, last}`.
  Events: `export_progress {id, key, preset, pass, frames_done, frames_total}` (at most
  every 250 ms or on a new pass; the bus drops when flooded, so `export.status` is the
  truth) and one `export_done {id, key, preset, ok, path, error, summary}`; a finished
  export also emits `library_changed {reason: "exported"}` and gets thumbnails. Encoders
  come from `config::export_encoders` (the user's recording encoder for its own codec,
  the ranked list otherwise; H.264 required, HEVC/AV1 when present). The limit for the
  Discord preset is `settings.export.discord_limit_mb` (decimal MB). Engine shutdown
  cancels a running export. Projects, proxies and renders follow a clip's rename and
  delete and are pruned with the thumbnails (`Library::{move,remove,prune}_caches`).
- Tests that make the engine probe encoders (an export, the setup) call
  `config::test_overrides(Some(<temp file>), true)` first: probe results go to that file
  instead of the user's cache and probing runs in-process (a test binary has no probe
  worker; isolated probes would all "fail" and be remembered as such).
- Media worker (`media_worker.rs`): library thumbnails and exports in a child process —
  the host binary re-run with `--klip-media-worker` (`worker_main()` first in `main`,
  after `klip_probe::worker_main()`), one JSON job per line (`Job`: `ThumbJob`,
  `ExportJob`, `CancelJob`, told apart by shape), atomic outputs, below-normal priority
  (Windows), stopped after 20 s idle. Thumbs: one `ThumbReply` each, 60 s then kill +
  respawn. Exports (M8.3, `klip-export`): run on their own thread in the worker so a
  `{"cancel": id}` line can stop them; `ExportReply` progress lines (every 15 frames)
  and one final line with the `ExportSummary`; the host (`MediaWorker::export`) polls
  its cancel flag, forwards progress, and replaces the worker after
  `EXPORT_INACTIVITY` (120 s) of silence. `EncoderChoice`s cross the boundary as
  element names (`klip_export::Encoders::names/from_names`); the worker runs
  `klip_probe::init` before encoding (Media Foundation before NVENC, ADR 0009) and
  decodes in software (`prefer_software_decoders`). `EngineOptions::media_worker =
  None` runs jobs in-process (tests).
- `events`: `Event` (`ready`, `game_started/stopped`, `recorder_started/stopped/failed`,
  `clip_saved` with the game's window, `recording_started/stopped`, `bookmark`,
  `action_failed` for hotkeys, `quick_menu`, `mic_level`, `thumbnail`, `overlay_preview`, `hotkeys` with each binding's error,
  `settings_changed`, `storage_cleaned`, `recovered`, `warning`), fanned out to bounded queues (256 per
  subscriber; a stalled subscriber loses events, memory doesn't grow).
- `config`: settings → `RecorderConfig`, shared with `klip-cli record`: clip and
  recordings folders, encoder choice + probe cache (`encoders_for`; `encoders_named`
  resolves names a recorder process gets against the installed, allowed encoders),
  target GPU and refresh rate, audio
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
is a hardware test on the default microphone). `tests/library.rs`: the library commands
on a temp folder (listing, filters, strict arguments, thumbnail failures not retried,
star/tags/rename keep together, delete, hostile paths). `engine/host.rs` unit tests:
replies routed by id, stray and invalid output ignored, one start reply only, an
over-long line closes the link and releases waiters, worker error codes mapped and
capped. Recorder process with hostile input: `apps/klip-cli/tests/workers.rs`. End to
end with the test game: `apps/klip-cli/tests/m6_daemon.rs` (recorder process, overlay
thumbnail, a killed recorder restarted and its recording finalised, no worker left and
memory back after the game), `m7_library.rs`.

Layer: media engine (no UI).
