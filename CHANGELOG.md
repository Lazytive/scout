# Changelog

Every change, with who made it and why. Newest first. Format: `task — summary (author:
reason)`.

## Unreleased

- M8.5 — The Editor workspace in the app (Normal mode): `src/Editor.svelte`,
  `src/editor/{Preview,Filmstrip,ExportDialog}.svelte`, `src/lib/forms/Slider.svelte`;
  state in `src/lib/editor.svelte.ts` (projects saved after every edit, undo/redo,
  playhead and selection in output time, exports through the events); the timeline
  math ported to `src/lib/timeline.ts` and checked against the shared fixtures
  (`timeline.test.ts`), helpers in `src/lib/editor.ts` (`editor.test.ts`); the mock
  engine's editor (`mock-editor.ts`: projects, SVG text, exports that tick and finish,
  a Discord second pass, an occasional failure); the Library's double-click, Enter,
  Edit button and menu open the editor; `app.page` in the store; the engine command
  `editor.text.render` (the text layer as the export draws it, base64 PNG) for the
  preview; `KLIP_START_PAGE=editor` + `KLIP_START_CLIP` for driving it off-screen; the
  `diag_make_clip` diagnostic in `klip-export` writes a real clip for that. (Claude
  Fable 5.1: M8.5)

- M8.4 — Editor and export commands: `editor.project.get|set` (project file
  `.klip/projects/<key>.json`, repaired, clip length wins), `editor.proxy` (H.264 clip →
  itself; else an H.264 proxy made by an export job with the new `Preset::Proxy`, in
  `.klip/proxies/`), `export.start|cancel|status` (one job at a time on its own
  thread through the media worker; `<name> - <Preset>.mp4` next to the clip via
  `Library::unique_sibling`, never overwriting; events `export_progress` throttled to
  250 ms and `export_done`; a finished export is indexed and thumbnailed);
  `config::export_encoders`; `settings.export.discord_limit_mb` (1–1000, default 10)
  with UI mirrors; `klip-library` per-clip caches (projects, proxies, renders) that
  follow rename and delete and are pruned with the thumbnails; Tauri `protocol-asset`
  enabled with an empty scope and the `editor_media` command that allows exactly the
  one file the engine names for the preview (and forbids the last). Engine tests on
  temp folders: project round trip with hostile args, rename keeps the project, delete
  removes it, an export job on an undecodable clip ends with `export_done {ok:false}`
  and no file, the Discord limit setting. (Claude Fable 5.1: M8.4)

- M8.3 — Export pipeline: `klip-export` (engine layer; `export(&ExportRequest,
  progress, cancel)`: presets Original/YouTube/TikTok/Discord, one `decodebin`
  pipeline seeked per segment at the segment's rate with `videocrop` →
  `videoconvertscale` → BGRA and `scaletempo` → S16 per audio track, output frames
  picked by `Render::frame_source_ms` (the preview's integer mapping), text cues
  blended on the CPU, the recorder's `build_encode_chain` + `build_aac_chain` behind
  `appsrc`s, Game/Microphone (or Mix) mixed with the sliders, `write_mp4` with
  bookmarks as chapters, Discord size-fit passes (`fit.rs`), atomic `.part` write,
  progress every 15 frames, cancel per frame). `klip_edit::plan::Render`
  (`SourceInfo`, `validate`, `frame_out_ms`/`frame_source_ms`/`frame_count`,
  `texts_at`, markers mapped to output time; `timeline::source_time_in/output_time_in`).
  `klip-capture`: `EncodeConfig::{max_bitrate_kbps, leaky_queue}`, `make`/`capsfilter`/
  `props` public. `klip-mp4`: `Mp4Info::audio_names` (handler names). Media worker:
  `Job` enum (`ThumbJob` | `ExportJob` | `CancelJob`), export threads with progress
  lines and cancel, `MediaWorker::export`. Found on the way: an audio sink added to
  an encode pipeline that only gets data after the video must be `async=false`, and
  one `decodebin` per stream deadlocks preroll (shared queue needed). A three-lens
  review with adversarial verification (22 findings) led to: non-blocking `appsrc`
  pushes that watch the bus (a failed encoder can no longer hang the export), the
  encode pipeline set to NULL when it fails to start (no leaked NVENC/MF sessions),
  plans checked against the probed clip length and capped at 4 h / ~4 GB, the last
  frame held across segments, a `.part` of another export left alone, a leading audio
  gap padded and the tempo filter drained (+100 ms seek tail), audio pushed late by
  the AAC delay (no negative origin on Linux), the YouTube box by the shorter side,
  no letterboxing from even-rounding, a reachable `clamp` panic removed, Discord
  stopping at once when another pass can't be smaller; new hardware checks: tall-crop
  columns, the 480p30 step-down on a 1080p clip, chapters and beeps through cuts and
  speed; deferred items in `plan/IMPROVEMENTS.md` G. (Claude Fable 5.1: M8.3)

- M8.2 — Text rasterisation in `klip-edit` (`text::render`): bundled Inter 4.1 Bold +
  ExtraBold (OFL, `assets/fonts/`, ATTRIBUTION.md), `skrifa` for glyph outlines
  (`ab_glyph` was tried first: its `ttf-parser` is unmaintained, RUSTSEC-2026-0192,
  so `cargo deny` refused it), `ab_glyph_rasterizer` for coverage, `png` for the
  bitmap; word wrap, max 3 lines, auto-shrink to 4 px then cut; pill,
  shadow (new `TextStyle::shadow`, on `bold` and `caption`), disc outline, fill;
  `TextBitmap` cropped to the drawn area with its frame position, `measure`,
  `cache_key`; golden hashes of the presets at two sizes, hostile strings (control
  characters, RTL, emoji, joiners, 300-character texts) and random strings/bytes/frames
  never panic. A three-lens review with adversarial verification found and fixed: the
  box blur dropped the first samples of each row/column (visible when a shadow touches
  the frame edge), empty lines and glyph-less texts got a pill, uppercase could exceed
  the 200-character cap, the shrink loop could stop before the minimum size, a tall
  block never shrank, hostile style values could cost seconds in the outline kernel.
  (Claude Fable 5.1: M8.2)

- M8.1 — Editor model: ADR 0017 (own export pipeline instead of GES, preview via Tauri
  `asset:` with an H.264 proxy fallback for HEVC, Klip-rendered text bitmaps shared by
  preview and export, exports in the media worker, Discord size-fit) and the pure crate
  `klip-edit`: project schema v1 (`.klip/projects/<key>.json`; lenient load that never
  fails, atomic save, unknown keys kept for Advanced mode), timeline math (pieces with
  0.5/1/2 speed, output segments, output and source time both ways, split/trim/cut/
  set_speed in output time; integer, checked), 9:16 format crop, text presets as data,
  `Shorts` template; shared fixtures `crates/klip-edit/fixtures/*.json` for the front
  end's port. Also: M6.12 merged into main (fast-forward, user decision); a code-review
  backlog in `plan/IMPROVEMENTS.md` (private). (Claude Fable 5.1: M8 start)

- M6.12 — The recorder runs in a child process (ADR 0016; branch
  `opus/m6.12-recorder-process`, worktree `klip-m6.12`, done in parallel with M7 and
  rebased onto it). `klip-engine`: `worker.rs` (`--klip-recorder`: start request with
  settings, game and encoder names → `config::plan_with` + `Recorder::start`; serves
  `klip_control` requests on threads, `worker.status`, `worker.finish`; end of input
  finalises a running recording), `engine/host.rs` (`Isolation`, `RecorderHandle`,
  `ChildRecorder`: capped strict JSON lines, per-command timeouts, stderr passed on,
  `CREATE_NO_WINDOW`, waits for the process to exit on stop), `config::plan` split into
  `encoders_for` / `encoders_named` / `plan_with`; a dead recorder process is reported,
  restarted at once the first time per game, and its recording finalised by recovery
  after the 15 s quiet time; `engine.status.recorder_pid`; the overlay poster comes
  from a one-shot media worker instead of a decoder in the engine. `klip-capture`:
  thumbnails initialise only GStreamer (no Media Foundation warm-up, -0.75 s per
  worker) and `prefer_software_decoders()` for the media worker (poster 1.3 s → 0.13 s,
  strip 1.5 s → 0.44 s in a fresh worker). `klip-control`: `read_line_capped` public.
  `klip-recorder`: `session::RECOVERY_QUIET` public. CLI/app `main`s call
  `klip_engine::worker_main()`. Tests: `engine/host.rs` unit tests (routing, junk,
  over-long lines, error mapping), `apps/klip-cli/tests/workers.rs` (hostile start
  requests, GPL/unknown encoder names dropped), `m6_daemon` e2e (recorder is the
  daemon's child, kill → restart + recording finalised, no worker left, memory back),
  test helper `assert_no_other_test_game` (parallel agents ran the test game at the same
  time). Bench: process-tree sampling, save round trip, thumbnail and recorder-start
  times, idle sampled after the start-up transient. Measured before/after: **381 MB →
  22 MB private after a game**, saves 85–94 ms, fps cost unchanged. ADR 0016 (0015 is
  M7's). (Claude Opus 5.5: engine kept ~380 MB after every game, measured in M6.10)

- M7.1–M7.6 — Library (ADR 0015). `klip-mp4::probe`: clip info from the `moov` of any
  MP4 (faststart, moov at the end, fragmented), hostile-input safe. `klip-library`: tags,
  rename (safe names, never overwrites, stars/tags/thumbnails follow), the rebuildable
  info index `.klip/index.json`, `query` (search, game/starred/tag/date filters, sorts,
  facets), thumbnail cache paths (collision-free names) and pruning. `klip-capture`:
  hover-scrub strips (16 key frames in one JPEG). `klip-engine`: media worker child
  process for library thumbnails (`--klip-media-worker`, timeout kill, idle stop, below-
  normal priority), `library.list|thumbs|thumb`, `clip.rename|tags|delete|open|reveal`,
  events `library_changed` / `thumbs_ready`, thumbnail queue (visible cards first, the
  rest only while no game is recorded); `games.detect_stores` (tests no longer follow a
  game the human is playing: an M7 e2e run had followed one). `klip-platform`:
  `open_file`, `reveal_file`. App: Library workspace (filters, day-grouped grid, hover-
  scrub cards, search/sort/size, multi-select + keyboard, details with rename/tags/info,
  context menu, delete confirm with Recycle Bin / permanent), `klipthumb` URI scheme,
  `KLIP_START_PAGE` test override, Settings → Games "Find games from store libraries".
  Tests: proptests for names/tags/MP4 probing, engine `tests/library.rs`, vitest
  `library.test.ts`, e2e `m7_library.rs`. (Claude Opus 5.5: M7)

- M6.11 — M6 wrap-up: ADR 0014 accepted with everything M6 decided (detection, storage,
  shell, look and themes, settings UI, overlay, benchmark method), ROADMAP M6 → done with
  an honest list (memory after a game, overlay vs VRR unmeasured, Linux paths unrun) and
  the human checklist, stale README lines fixed. (Claude Opus 5.5: M6 done)

- M6.10 — `cargo xtask bench` (`apps/klip-cli/tests/bench.rs`): GPU-load calibration,
  uncapped and 144-fps-capped scenarios, interleaved Klip off/on pairs with a 95 %
  interval, engine CPU/RAM, 30 s save latency, idle cost; JSON in `target/bench/`.
  Test game: `--uncapped`, `--gpu-load N`. WGC capture asks for at most 2x the recording
  rate (`MinUpdateInterval`). e2e: `KLIP_E2E_MONITOR=offscreen`. Measured: no cost at
  144 fps capped, 2.9 % ± 1.0 % uncapped, 6 % of one core, 379 MB private after a game.
  (Claude Opus 5.5: M6 performance budget)

- M6.9 — Overlay: separate transparent, always-on-top, click-through window that is
  never activated (`klip-platform`: `make_overlay`, `show_no_activate`, `hide_window`,
  `focus_window`, `window_visible`), on the game's monitor corner; Minimal card with
  thumbnail (on by default, the maintainer's choice), Full (recording dot, quick menu),
  Off/exclusive fullscreen → sound/notification only; synthesised chime (cpal),
  notifications (`tauri-plugin-notification`). Thumbnails: `klip_capture::thumbnail`
  (decodebin → JPEG), engine job + cache + `thumbnail` event; `overlay.preview` and a
  "Show me" button. e2e `apps/klip-app/src-tauri/tests/overlay.rs` (off-screen): shows
  in ≤ 11 ms, 2.00 s, never focused, frame pacing unchanged. `xtask e2e` builds the
  front end first. (Claude Opus 5.5: M6)

- M6.8 — Settings workspace (General, Recording, Audio, Hotkeys, Games, Storage, Overlay)
  and first-run setup in the app; form controls in the Photocraft style; strict saves
  with per-field errors. Engine: `encoders.list`, `audio.devices`, `audio.meter` +
  `mic_level`, `recording.estimate`, `hotkeys.pause|resume`, `games.candidates`
  (`games::candidates`). App: folder picker (`tauri-plugin-dialog`), start with Windows
  (`tauri-plugin-autostart`, applied on change only), the built front end embedded by
  default (`custom-protocol`; `--no-default-features` for hot reload). `tools/
  window-shot.ps1` captures one window (PrintWindow) for UI checks. (Claude Opus 5.5: M6)

- M6.7 — Default theme is now "follow system" (maintainer's choice); `app.css` paints
  Light first when the OS is in light mode (kept equal to Light by the theme tests).
  (Claude Opus 5.5)

- M6.7 — Themes: Dark (FilmCraft greys, default), Graphite (Photocraft greys, new in
  `klip-settings`), Light, OLED black, follow system; colour tokens swapped at run time,
  native title bar follows, header theme menu, live on `settings_changed`. vitest
  contrast suite (WCAG AA for every text pair, 3:1 for outlines/focus/state colours, all
  themes); primary blue darkened to #1f6fe0 and outlines lightened so Dark passes.
  `klip-cli call COMMAND [JSON]` runs any command. (Claude Opus 5.5: M6; look requested
  by the maintainer)

- M6.6 — Desktop app `apps/klip-app` (Tauri 2 + Svelte 5 + TS): engine bridge (`klip`
  command, `klip://event`), tray with runtime-drawn state icons and menu, single
  instance, close to tray, clean shutdown, test overrides (`KLIP_SETTINGS`,
  `KLIP_DISCOVERY`, `KLIP_WINDOW_POS`), Home workspace (Monitor, Info/Storage,
  Activity/Hotkeys) in the Photocraft/FilmCraft look, mock engine for browser work, own
  app icon. `cargo xtask ci`: eslint, svelte-check, vitest, front-end build, npm licence
  audit, icons in the attribution check; deny ignores RUSTSEC-2024-0370 (Tauri's GTK3
  stack, build-time). (Claude Opus 5.5: M6; look requested by the maintainer)

- M6.5 — Storage management. New pure crate `klip-library` (clip-folder metadata with
  stars in `.klip/library.json`, scan, the cap policy). `klip-platform::move_to_trash`
  (`trash` crate, MIT). Engine: clean-up after saves / at start / on settings change,
  low-disk warning, commands `storage.status`, `storage.cleanup`, `clip.star`, event
  `storage_cleaned`. (Claude Opus 5.5: clips must not fill the disk; starred ones are
  never touched)

- M6.4 — `klip-engine`: `Engine` (settings, `klip-watch` game watcher, recorder start/stop
  per game with profiles and per-game clip folders, Always/Games/Off buffer modes, retry,
  recovery of leftover recordings, hotkeys from settings with per-binding errors, job
  threads, strict `settings.set`), `events` (bounded fan-out), `config` (settings →
  `RecorderConfig`; the CLI's encoder/audio/portal helpers moved here). New commands in
  `klip-control`. `klip-platform`: monitor refresh rate, `process_usage`. `klip-cli
  daemon`. Tests: engine commands/settings (no recording), e2e `m6_daemon.rs`
  (detected → hotkey clip continuous in its game folder → stops after exit; idle CPU
  0.00 %). (Claude Opus 5.5: one engine for the app, the CLI and the tests)

- M6.3 — New crate `klip-engine` with `games`: game detection from process/window
  snapshots (user list, store library folders, helper/launcher denylist, never list,
  window size, foreground first; window-less mode for Wayland incl. Proton paths), safe
  per-game folder names. One poll measured at 3.6 ms median. (Claude Opus 5.5: buffer
  only while a game runs)

- M6.2 — `klip-platform`: Windows global hotkeys (`RegisterHotKey` on its own message
  thread behind `GlobalShortcuts`; per-shortcut `error` for taken/unknown keys),
  shortcuts-spec trigger → virtual-key map, `press_keys` (SendInput, for tests),
  `process_path` (Windows: PROCESS_QUERY_LIMITED_INFORMATION; Linux: /proc, Wine
  command line), public `list_windows`. `klip-cli --hotkey/--record-hotkey/
  --bookmark-hotkey` now work on Windows. (Claude Opus 5.5: M6 hotkeys and game
  detection)

- M6.1 — ADR 0014 (app architecture: `klip-settings`, `klip-library`, `klip-engine`,
  thin Tauri shell; settings file rules; presets; hotkeys via RegisterHotKey on Windows).
  New pure crate `klip-settings`: settings schema v1, hostile-file loading with
  field-by-field repair and `.bad` backups, strict merge-patch changes, per-game profiles,
  quality presets, hotkey syntax (both formats). Unit + property tests. (Claude Opus 5.5:
  M6 needs one settings model shared by the engine, CLI and UI)

- M5.1–M5.6 — Full-session recording + bookmarks (ADR 0013). `klip-mp4`: shared `moov`
  code (`moov.rs`, chunk tables), `FragmentedWriter`, `scan_fragmented` (hostile-input
  scanner), `finalize` (in place, crash-safe order, repeatable), markers as `udta/chpl`
  chapters in clips and recordings. `klip-buffer`: `tail_from_last_keyframe`.
  `klip-platform`: `free_disk_space`. `klip-recorder`: `session` (writer thread fed under
  the ring lock, fragments at keyframes, sidecar with bookmarks, disk/queue limits,
  rollover, stop cut at the video end, `recover_recordings`, `finalize_recording`),
  recorder bookmarks, commands `session.start|stop|toggle`, `bookmark.add`, status
  `session`. CLI: `record --session --session-dir --record-hotkey --bookmark-hotkey`,
  `session start|stop|toggle`, `bookmark`, `finalize FILE`, recovery at start. Tests:
  unit (truncation, interrupted finalise, mutation, recovery), hardware
  `synthetic_session.rs`, e2e `m5_session.rs` (hard kill → playable → finalised with
  chapters; normal stop with A/V sync). Windows gates re-run after the M4 Linux changes:
  green. (Claude Opus 5.5: M5)

- M4.4/M4.6 — Per-app PipeWire packets stamped one buffer later (they arrive one graph
  cycle before the output's mix); per-game A/V now +0.07 to −11.9 ms. M3 window/DXGI/HDR
  tests are Windows-only; `cargo xtask e2e` green on Linux. Linux manual checklist in
  ROADMAP. (Claude Opus 5.5: M4)

- Fix — audio timeline: a packet that resumes after generated silence but starts later
  than it (PipeWire after a quantum/driver change: 41.6 ms) left a gap; the gap is now
  filled with silence (jumps over 10 s are flagged as discontinuities instead). Found as
  a flaky `audio_capture_is_continuous_and_qpc_stamped`; regression tests in
  `timeline.rs`. (Claude Opus 5.5: never lose audio continuity)

- M4.5 — Hotkey on Wayland: `klip_platform::GlobalShortcuts` (GlobalShortcuts portal,
  own thread), `register_app_id` (host-app registry + hidden desktop entry, working id
  `dev.klip.Klip`); CLI `record --hotkey [KEYS] --hotkey-length`; verified by hand on KDE
  (Alt+F10 → clip). (Claude Opus 5.5: M4)

- M4.4 — Linux audio (ADR 0008): `AudioCapture` over PipeWire (output monitor, input,
  per-app streams linked by Klip with include/exclude, process ids from
  `pipewire.sec.pid`), `audio_sessions`, shared silence timeline (`timeline.rs`);
  AAC via `fdkaacenc`/`avenc_aac` with measured encoder delays; lag estimator handles
  early streams (Linux) with a 0 default; CLI `--game EXE`; test game tap on Linux via
  Klip's PipeWire capture. (Claude Opus 5.5: M4)
- M4.3 — Portal timing and lifecycle (ADR 0012): frames stamped on arrival minus a
  measured compositor latency (KDE 24 ms), A/V −5.1 ms; the desktop ending the
  screen-cast session is reported as a capture error (portal session owned by its own
  thread); `encoder_fallback` runs on Linux. (Claude Opus 5.5: M4)

- M4.3 — Linux video capture (ADR 0012): `klip_platform::ScreenCast` (xdg-desktop-portal
  ScreenCast via `ashpd`, persist "until revoked", restore token), `VideoTarget::Portal`
  + shared `PortalSession`, `pipewiresrc` → `videorate` → encode chain sized from the
  stream's first caps; CLI `--portal`, `--portal-token` (token stored atomically, read
  back as hostile input); test game `--monitor NAME|secondary` (Wayland can't place
  windows); M1 acceptance passes through the portal on KDE Wayland. (Claude Opus 5.5:
  M4, the user is on Linux)
- M4.2 — Linux platform basics + encoders: GPUs from sysfs, processes from `/proc`,
  `qpc_now_ns` = CLOCK_MONOTONIC; encode chain from system memory (GL convert/scale →
  NVENC, `vapostproc` → VA-API, `videoconvertscale` → openh264); VA low-power encoders
  and per-device names on the allowlist; openh264 probes convert to I420. All four Linux
  encoders round-trip exactly. (Claude Opus 5.5: M4)
- M4.1 — Linux builds: `cargo xtask ci` green on Bazzite in a Fedora 44 distrobox.
  (Claude Opus 5.5: M4)

- M3.6 — Capture fallbacks (ADR 0011): recorder supervisor switches window capture to
  its monitor on exclusive fullscreen, a stall while covering the monitor, or a capture
  failure, back when the window leaves fullscreen, and to DXGI when monitor capture sees
  nothing in exclusive fullscreen; restarts keep encoder and output size
  (`VideoConfig::output`) so the buffer continues; uncapturable windows record their
  monitor from the start. `--capture-api gst-dxgi` uses `d3d12screencapturesrc` (exact
  SDR on HDR desktops). Test game `--freeze FROM..TO`, borderless on the `--position`
  monitor; `tests/m3_fallback.rs`. (Claude Opus 5.5: M3)

- M3.5 — HDR: on a monitor in Windows HDR mode the own capture records FP16 scRGB and
  tone-maps to SDR in the same shader pass (SDR white = Windows' SDR content
  brightness, GPU-measured scene peak capped at the display's peak, highlight roll-off
  above a knee in sRGB-encoded space). `tests/m3_hdr.rs`: SDR content exact, HDR
  highlights distinct up to the display peak. (Claude Opus 5.5: HDR games were clipped
  to flat white)
- M3.4 — Own WGC capture (`klip_platform::GpuCapture`, ADR 0010): D3D11 device on the
  encoder's GPU shared with GStreamer through the gst-d3d11 C API, client-area crop /
  scale / letterbox in one shader pass, frames stamped when they arrive, bounded by WGC's
  `SystemRelativeTime` (measured: that is the display time, ~1 refresh later; using it
  put video 9–13 ms late);
  default `--capture-api auto` falls back to `d3d11screencapturesrc`. Status reports
  `capture_api` and capture counters. (Claude Opus 5.5: M3)
- Tests — the test game takes `--position X,Y`; e2e tests open it on a secondary monitor
  (`KLIP_E2E_MONITOR` to choose). `KLIP_E2E_CAPTURE_API` runs the e2e tests with
  another capture implementation; `oracle::decode_gray_frame`. (Claude Opus 5.5: the
  human games on the primary monitor while tests run)

- M3.2/M3.3 — AV1 clips: `klip_media::av1` builds the `av1C` record from the sequence
  header in the first keyframe (`av1parse` caps carry none), so NVENC AV1 records into
  MP4; AV1 had never been run end to end. (Claude Opus 5.5: found by the all-encoder
  round trip)
- M3.2 — Encoders on Windows can't share a process freely (ADR 0009): `klip_probe::init`
  starts Media Foundation before any NVENC session (else `mfaacenc` starting after NVENC
  breaks every later NVENC session: `NV_ENC_ERR_INVALID_VERSION`), D3D12 ranks before
  hardware MFTs (a session on NVIDIA's MFT breaks later NVENC sessions), probes run in a
  child copy of the app (`klip_probe::isolated`, `--klip-encoder-probe`). Regression test
  `nvenc_survives_media_foundation_in_between`. (Claude Opus 5.5: in-process probes
  reported working NVENC as broken)
- M3.2 — Encoder selection: D3D12 Video Encode family (`d3d12h264enc`, vendor-neutral),
  per-device element names for all families, ranking by the GPU vendor of the captured
  monitor (hybrid laptops), probe cache (`%LOCALAPPDATA%\Klip\encoders.json`,
  fingerprinted by GStreamer version, adapters + driver, installed encoders), recorder
  start falls back to the next encoder when one fails before its first frame
  (`encoder_attempts` in status), CLI ranks for the target's monitor and keeps the
  cache. (Claude Opus 5.5: M3)
- M3.3 — Per-family encoder settings: MF peak-constrained VBR (`pcvbr`, default had no
  peak), D3D12 VBR, AMF/QSV from the GStreamer docs (QSV `idr-interval=0`);
  `every_working_encoder_round_trips_exactly` checks every installed encoder of every
  codec (14 here) for exact frames and keyframe cuts. Frames dropped by the encoder's
  leaky input queue are now counted (`encoder_queue_drops`). (Claude Opus 5.5: M3)
- M3.1 — Platform: GPUs deduplicated by LUID with driver version; monitors report their
  adapter, HDR state, SDR white level and peak luminance; windows report screen position
  and monitor (`WindowInfo::covers`); `exclusive_fullscreen_active`. Test game
  `--patches`/`--hdr`/`--paper-white` (FP16 scRGB swapchain) and `klip_testkit::patches`
  for colour and tone-mapping checks. (Claude Opus 5.5: M3; HDR measurements in
  execution plan)
- xtask — `e2e` runs with `--no-fail-fast` so one failing test binary doesn't hide the
  rest. (Claude Opus 5.5)

- M2.4 — App selection: `klip_platform::{list_processes, audio_sessions}`,
  `klip_capture::apps` (default list of music players left out of system audio,
  `plan_system`: whole mix / exclude one tree / per-app capture, `app_roots` so
  multi-process apps are captured once), `AudioSource::Apps`, CLI `--audio apps`, `--app`,
  `--exclude-app`, `--no-default-excludes`, `audio-apps`. (Claude Opus 5.5: keep other
  people's music out of shared clips)
- M2.3 — A/V sync acceptance: test game `--beep`, `--beep-log`, `--tap-log` (its own
  loopback measurement of each beep), `--background`, frame log after GPU completion;
  `klip-testkit` `audio` (beeps, onsets) and `sync`; `tests/m2_audio.rs` (sync ≤ 1 frame
  with per-stream stamp latency, mic track layout, excluded app left out); e2e helpers in
  `tests/common`; `diag_*` diagnostics skipped by `cargo xtask e2e`. (Claude Opus 5.5: M2
  acceptance)
- M2.3 — Video timing: the WGC source polls at 8x the capture rate, frames are stamped on
  arrival (kept in a `ReferenceTimestampMeta`), `videorate drop-only` returns to the
  capture rate, and kept frames are snapped to a frame grid that re-centres on the
  arrival phase. The source's own stamps were up to a frame late by a per-run amount.
  (Claude Opus 5.5: found by the sync test's per-stream breakdown)
- M2.1 — Audio capture rewritten: own WASAPI reader in `klip-platform` (`AudioCapture`:
  loopback, process loopback, input; QPC capture stamps; silence when idle; retry on
  failure), `klip_media::DriftAligner`, `appsrc` inputs, live process-loopback lag
  measurement against the system mix (`klip-capture/src/lag.rs`). Replaces
  `wasapi2src`, whose arrival-time stamps put audio −20 to +23 ms off, differently per
  run and source type. (Claude Opus 5.5: sync measurements)
- M2.1 — Fix: heap corruption in process-loopback activation: the `windows` crate's `Drop`
  for `PROPVARIANT` freed the stack blob; now `ManuallyDrop`. Regression test:
  `audio_capture_is_continuous_and_qpc_stamped`. (Claude Opus 5.5)
- M2.2 — AAC tracks: `build_aac_chain` (`mfaacenc` → `aacparse`), AAC caps → `TrackInfo`,
  `tests/synthetic_audio.rs` (beeps land within 0.01 ms; no encoder-delay correction
  needed), MP4 track names / enabled flag / audio alternate group, ring snapshots keep
  track indices, ADR 0005 accepted, ADR 0008. (Claude Opus 5.5: M2)
- M2.2 — Saves cut at the request time and wait up to 250 ms for audio to cover the end
  (`Ring::snapshot_until`); audio used to stop ~60 ms before the video. (Claude Opus 5.5:
  found in the first audio clips)
- M2.1 — Recorder audio tracks and status (`audio`, `clock_base_ns`, clip `start_ns`),
  shared `TimeBase` for all pipelines, CLI `--audio`, `--mic`, `--audio-bitrate`,
  `--no-separate-tracks`, `audio-devices`. (Claude Opus 5.5: M2)
- M2.1 — Fix: NVENC output timestamps carried a 1000-hour offset (GStreamer `min-pts`);
  harmless for video-only clips, but every audio packet was evicted as "older than the
  video". The first encoder input time is now recorded and packets moved back
  (`InputOrigin`), with a regression test. (Claude Opus 5.5: found by the audio smoke test)
- M2 — Video timestamps snapped to the capture frame grid (`klip_media::FrameGrid`):
  constant frame rate where no frame is missing, all frames kept. (Claude Opus 5.5: CFR
  for editors)

- M1.12 — Acceptance: `apps/klip-cli/tests/m1_acceptance.rs` (test game + record + clip,
  ffprobe/ffmpeg oracle, frame continuity) and `crates/klip-recorder/tests/synthetic_encode.rs`
  (exact frame round trip through NVENC, ring, MP4). (Claude Opus 5.5: M1 acceptance)
- M1.12 — Snapshot treats a keyframe ≤ 1 ms after the cut target as at the target, with a
  regression test; truncated 1/60 s timestamps had added a whole extra GOP. (Claude Opus
  5.5: found by the synthetic test)
- M1.11 — `klip-cli`: record, clip, status, stop, probe, monitors, windows. (Claude Opus 5.5)
- M1.10 — `klip-recorder`: ring sink, atomic clip saves, status, command handler. (Claude
  Opus 5.5)
- M1.9 — `klip-control`: JSON-lines loopback control channel with token file. (Claude Opus 5.5)
- M1.8 — `klip-capture`: WGC → d3d11convert → NV12 → encoder → parser → appsink pipeline,
  panic-free property setting. (Claude Opus 5.5)
- M1.7 — `klip-probe`: encoder allowlist/denylist, ranking, dummy-encode probe. (Claude
  Opus 5.5)
- M1.6 — `klip-platform`: DXGI adapters, windows, monitors, DPI awareness (only crate with
  `unsafe`). (Claude Opus 5.5)
- M1.5 — `klip-testgame`: D3D12 wgpu window with barcode, motion, flash; self-paced fps.
  Rendering moved out of `RedrawRequested` because WM_PAINT scheduling made it miss 120 fps.
  (Claude Opus 5.5)
- M1.4 — `klip-testkit`: barcode spec/renderer/reader, ffmpeg oracle, continuity
  analysis. (Claude Opus 5.5)
- M1.3 — `klip-mp4`: faststart MP4 writer. (Claude Opus 5.5)
- M1.2 — `klip-buffer`: replay ring. (Claude Opus 5.5)
- M1.1 — `klip-media`: shared media types. (Claude Opus 5.5)
- M0 — Repository, rules (`AGENTS.md`), ADRs 0001–0007, workspace lints, `cargo xtask`
  gates (ci, layers, audit, e2e). winit built without the Adwaita decorations feature so
  `cargo deny` stays clean (it pulled the unmaintained `ttf-parser`). (Claude Opus 5.5:
  initial setup per the build brief)
