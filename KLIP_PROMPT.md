# Klip: build prompt

To start: open a fresh Claude Code session in `C:\Users\GodHead\Claude\klip` and say "Read KLIP_PROMPT.md and follow it."

---

You are building **Klip**: a free, open-source, public game-clipping app in the spirit of Medal.tv, written in **Rust**, for **Windows and Linux**, supporting **NVIDIA, AMD and Intel** GPUs. You work autonomously in the style of storytold/filmcraft (https://github.com/storytold/filmcraft): a private plan with task ids, hard rules in `CLAUDE.md`/`AGENTS.md`, quality gates before every commit, and an app the agent can drive and verify by itself.

"Klip" is a working name. Before any public release, check it for conflicts (GitHub, app stores, trademarks) and ask me.

## 0. First session

1. Check prerequisites: Rust stable (MSVC), GStreamer ≥ 1.24 MSVC runtime + development packages, Node LTS (Tauri front end), WebView2 (preinstalled on Windows 11). List what's missing and **ask before installing anything system-wide**.
2. Do **M0** (repo setup), then **M1** (recorder CLI) on this machine. Detect my GPU and start with its encoder.
3. Stop when M1's acceptance tests pass. Report briefly: what works, what was measured, what's next. Leave a handoff in `plan/STATUS.md` so the next session can continue without this conversation.

## 1. Hard rules (copy into `AGENTS.md`; they override everything)

1. **Anti-cheat safe, always.** Never inject into a game process, hook its rendering (DirectX/Vulkan/OpenGL present hooks), read or write its memory, or install drivers. Capture only through OS APIs: Windows Graphics Capture with DXGI Desktop Duplication as fallback; on Linux, the xdg-desktop-portal ScreenCast portal + PipeWire. Game events come only from official game integrations or from analysing frames Klip already captured.
2. **Clean-room.** Never decompile, disassemble or unpack Medal or any other clipper (Overwolf/Outplayed, ShadowPlay, Steam recording, OBS builds). Black-box observation of public behaviour only. No Medal name, logos, icons, sounds or copied UI artwork. Never copy code from GPL projects (OBS, gpu-screen-recorder, x264, GPL ffmpeg builds). Public docs and specs are fine.
3. **Never crash, never lose a clip.** No `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`unreachable!` outside tests. `unsafe` only in one `platform` crate, each block justified. Return `Result`, propagate with `?`. Treat every input as hostile: media, config files, theme files, game-integration JSON, IPC messages. Use checked math, `get()` not `[i]`, capped allocations. Background jobs run under `catch_unwind`; locks use `unwrap_or_else(PoisonError::into_inner)`. Writes are atomic (temp file + rename). Every crash fix gets a regression test that failed before.
4. **Privacy.** No account and no telemetry. The only network traffic is the update check (can be switched off) and sharing the user starts. Never log personal data or file contents.
5. **Licences.** Klip is MIT OR Apache-2.0. Allowed dependencies: permissive (MIT, Apache, BSD, ISC, Zlib, MPL-2.0) and LGPL **dynamically linked** (GStreamer). No GPL or AGPL dependencies or bundled GStreamer plugins (exclude x264enc, GPL builds of gst-libav, and so on). Enforce with `cargo deny`, plus an audit script for bundled GStreamer plugins and front-end npm packages. Every asset (icons, sounds, fonts, sample themes) is openly licensed, with an entry in `ATTRIBUTION.md`.
6. **Codec patents.** For H.264/HEVC, use only GPU vendor encoders, OS encoders (Media Foundation) and Cisco's openh264 binary on Linux (the Flatpak openh264 extension). Don't bundle other software H.264 encoders. Record audio-codec choices in an ADR.
7. **Measured, not guessed.** Performance and progress numbers must come from tests or benchmarks you ran, with machine and settings stated. A stub or approximation is not "done".
8. **Ask the human only about:** spending money (code-signing certificate, domains, hosting); pushing, publishing or releasing; licences outside the allowlist; personal data; destructive git operations; running tests in real anti-cheat games; the final name. Decide everything else yourself and record it in an ADR or the crate README.

## 2. Stack

| Area | Choice | Notes |
|---|---|---|
| Language | Rust workspace (`crates/*`, `apps/*`, `xtask`) | |
| Capture + encode | **GStreamer via gstreamer-rs** (official bindings) | Windows: `d3d11screencapturesrc` (WGC, window or monitor; DXGI fallback; no yellow capture border on Win 11), `wasapi2src` (loopback, per-process where supported). Encoders: NVENC (`nvh264enc`/`nvh265enc`/`nvav1enc`), AMD AMF (`amfh264enc` …), Intel QSV (`qsvh264enc` …), Media Foundation `mfh264enc` fallback. Linux: portal ScreenCast via `ashpd` → `pipewiresrc`, VA-API (`vah264enc`/`vah265enc`) for AMD/Intel, NVENC for NVIDIA, PipeWire/Pulse monitor sources for audio. **Verify element names and properties against the installed GStreamer; don't trust this table blindly.** |
| Encoder probe | Own crate | At first run, try each available encoder with a 1-second dummy encode. Pick the best working one and keep a ranked fallback list. Handle hybrid laptops (iGPU + dGPU): encode on the GPU that owns the captured frames, or copy explicitly. |
| Replay buffer | Own crate `klip-buffer` | A ring buffer of **encoded packets** (video + each audio track), bounded by duration and bytes. Keyframe every 1 s. Saving finds the keyframe at or before the start and remuxes to MP4 **without re-encoding**. Target: save in under 1 s. |
| Containers | MP4 | Clips: faststart (moov first). Full-session recordings: fragmented MP4 while recording so a crash leaves a playable file, finalised on stop. |
| Desktop app | **Tauri 2**: Rust backend, web front end (TypeScript + Svelte *or* Solid, decide in an ADR) | Tray icon, windows, updater (signed update manifests), installers. |
| Global hotkeys | Tauri global-shortcut plugin on Windows/X11; the xdg-desktop-portal **GlobalShortcuts** portal on Wayland | Fallback everywhere: a `klip clip` CLI command users can bind to a desktop shortcut. |
| Overlay | A separate transparent, always-on-top, click-through window | Linux Wayland: layer-shell (gtk-layer-shell) where supported (KDE, wlroots), otherwise a system notification. |
| Editor export | GStreamer pipeline. ADR: GStreamer Editing Services vs a custom pipeline | The preview (web UI: `<video>` + overlays) must match the export. Add a parity test that compares frame N from both. |
| Later | `whisper-rs` (local captions), `nnnoiseless` (mic noise removal) | Both permissive. |

## 3. Features

### Recording
- **Replay buffer** (always on while a game runs). Default length 2 min; a RAM estimate is shown in settings.
- **Full-session recording** (start/stop hotkey) with a **bookmark** key that marks moments, which show up as markers in the editor.
- **Multiple clip keys:** e.g. last 30 s (default), last 2 min, plus bookmark. All rebindable.
- **Game detection:** buffer only while a known game (or a user-added exe) is running; near-zero CPU/RAM otherwise. Clips are tagged by game. Per-game profiles: buffer length, quality, audio sources, auto-clip on/off.
- **Audio:** game, Discord/voice apps and mic on separate tracks; choose which apps to capture; exclude music apps by default to avoid copyright strikes. A/V sync within one frame.
- **Quality presets:** Low (720p30, ~8 Mbps), Balanced (1080p60, ~20 Mbps, default), High (native up to 1440p, 60–144 fps, ~40 Mbps), Custom (resolution, fps, bitrate, codec H.264/HEVC/AV1 where the hardware supports it). Tune the numbers by measurement.
- **HDR:** tone-map HDR games to SDR (BT.2020 PQ → BT.709) on the GPU so clips don't look washed out. HDR10 output optional later.
- **Exclusive fullscreen:** if window capture fails, fall back to monitor capture automatically.
- **Storage management:** a size cap (default 50 GB, configurable). When over the cap, the oldest **unstarred** clips go to the Recycle Bin/Trash (default) or are deleted (setting). Starred clips are never touched. Warn when the disk is low.

### Feedback while playing
- **Overlay setting:** Off (sound cue and/or system notification), **Minimal (default)**: a small "Clip saved" pop-up with thumbnail for ~3 s, Full: pop-up + optional recording dot + a hotkey quick menu (recent clips, bookmark). Show the overlay only briefly; it must not cause lasting input lag or turn off G-Sync/FreeSync. Measure this.

### Library
- A grid with thumbnails and hover-scrub; search; filters by game, date and starred; star, rename, tags, delete (to trash).
- Clips are plain MP4s in a normal user-chosen folder. No lock-in, no proprietary database required to read them (sidecar JSON or SQLite index is fine, rebuilt from the folder if lost).

### Editor: two modes on the same project
Edits are non-destructive (the original clip is never changed). A mode toggle in the corner, remembered. Switching modes never loses work; advanced edits made in Advanced mode stay in Normal mode, shown with an "advanced edits" badge.

**Normal mode** (done in a minute; no tracks, no keyframes, no numbers):
- Preview, trim handles, split and cut middle parts out
- Text in a few good preset styles, dragged on the strip to set timing
- Format: 16:9, vertical 9:16, gameplay + webcam split (once webcam exists)
- Speed buttons on a selection: 0.5×, 1×, 2×
- Two sliders: game volume, mic volume
- Templates, e.g. "Shorts" = vertical crop + captions + zoom on the action
- Export presets: **Discord** (fits the current free upload limit, which is a setting; two-pass bitrate calculation), **YouTube**, **TikTok/Shorts**, **Original**

**Advanced mode:**
- Multi-track timeline: video, text, mic, game audio, music (with auto-ducking)
- Keyframes for zoom, position, volume; speed ramps
- Free crop/position with exact numbers; basic colour (brightness, contrast, saturation)
- Editable word-by-word captions (when captions exist)
- Transitions, stickers, sound effects
- **Montage:** several clips in one project
- Auto-clip events and bookmarks as timeline markers
- J/K/L, snapping, undo/redo with history
- Custom export: resolution, fps, bitrate, codec

### Themes (colour tokens from day one, even before the theme UI exists)
- v1: Dark (default), Light, OLED black, follow system; an accent colour picker; **custom theme files** (colours, corner roundness, font, optional background image) with import/export and a contrast warning; density (compact/comfortable), text size, reduced motion. The overlay follows the theme.
- Later: custom CSS for power users (marked unsupported, local files only, no remote URLs, enforced by CSP); an in-app community theme gallery.
- Layout rearranging only inside Advanced mode's editor, not app-wide.

### Sharing (server-free by default)
- v1: copy clip to clipboard as a file, drag out of the library, "Export for Discord".
- **Self-hostable share server:** a small Rust (axum) server in Docker. Klip uploads to a server the user configures and copies a link. Thumbnails and a simple player page; upload tokens; size limits; never public by default.
- Later: upload to YouTube / Streamable via their official APIs.
- Not planned: a Klip-run cloud (storage/bandwidth cost, moderation).

### Auto-clip
- v1: none (hotkeys only).
- Next: official integrations only. **CS2 and Dota 2 Game State Integration** (Klip installs a `gamestate_integration_klip.cfg` with the user's consent; the game POSTs JSON to a local endpoint), **League of Legends Live Client Data API** (local HTTPS, self-signed cert). On an event at time T, save T−20 s … T+5 s (configurable).
- Later: **vision game profiles**. Detect kill banners and kill-feed changes in captured frames with a small local model or template matching. Profiles are data files the community can contribute.
- Never: memory reading or anything anti-cheat could flag.

### App
- **First-run setup:** detect GPU and pick encoder, choose quality preset, set clip hotkey, test mic, choose clip folder, overlay style.
- **Tray app**, optional start with the OS.
- **Performance budget** (measured on the benchmark scene, Balanced preset): ≤ 3% FPS cost on a mid-range GPU, near-zero idle cost with no game running, RAM ≈ buffer + ≤ 200 MB.
- **Updates and installers:** Windows NSIS/MSI via Tauri; Linux **Flatpak** (primary; Bazzite uses Flatpak) and AppImage. Code signing for Windows: **ask me first** (costs money).
- Later: controller clip button, webcam capture, mic noise removal, local captions, Steam Deck/Bazzite Game Mode (gamescope).

## 4. Milestones

Each milestone has numbered tasks (`M1.1`, `M1.2`, …) in `plan/execution-plan.md`. Do them in order unless a task says otherwise.

| | Milestone | Acceptance (summary) |
|---|---|---|
| M0 | Repo, rules, gates | `CLAUDE.md`, `AGENTS.md`, `ROADMAP.md`, `plan/` (gitignored), ADRs for the stack, `cargo xtask ci` green. |
| M1 | Recorder CLI (Windows, your GPU first) | `klip-cli record --buffer 120s` + `klip-cli clip --last 30s` → valid MP4 in < 1 s; test scene passes frame-continuity checks. |
| M2 | Audio | Game, per-app and mic tracks; A/V sync ≤ 1 frame on the test scene. |
| M3 | All GPUs + fallbacks | NVENC, AMF, QSV, Media Foundation; encoder probe; hybrid laptops; exclusive-fullscreen fallback; HDR tone mapping. |
| M4 | Linux | Portal + PipeWire capture, VA-API/NVENC, PipeWire audio, GlobalShortcuts portal; tested on KDE and GNOME (Wayland) and X11. |
| M5 | Full-session recording + bookmarks | Fragmented MP4 survives a killed process. |
| M6 | Desktop app shell | Tauri tray app, first-run setup, settings, hotkeys, game detection + profiles, quality presets, storage management, overlay (Off/Minimal/Full), theme tokens + built-in themes. |
| M7 | Library | Grid, thumbnails, hover-scrub, search/filter/star/rename/tags, index rebuilt from folder. |
| M8 | Editor, Normal mode + export presets + templates | Preview/export parity test; Discord preset always fits the limit. |
| M9 | Editor, Advanced mode (incl. montage) | |
| M10 | Themes v1 | Custom theme files, accent picker, density, text size, reduced motion, contrast warning. |
| M11 | Sharing | Discord export, clipboard/drag, self-hostable share server (Docker). |
| M12 | Auto-clip: official integrations | CS2, Dota 2, League. Real-game tests only with my OK. |
| M13 | Release 0.1 | Installers, Flatpak, updater, README with screenshots made by driving the app, licence/asset audit clean. Ask before publishing. |
| Later | Captions, noise removal, webcam + facecam layout, controller button, vision profiles, custom CSS, theme gallery, YouTube/Streamable upload, Game Mode | |

## 5. How you verify your own work

- **Test game:** build `klip-testgame` (wgpu window). Each frame shows a machine-readable frame counter (e.g. a block barcode) and a moving pattern, with a beep + white flash every second. Settings for resolution, fps, windowed/borderless/exclusive fullscreen and HDR.
- **Clip checks** (automated tests): decode the saved clip, read the barcodes, and assert no dropped or duplicated frames, correct duration and fps, A/V sync (beep vs flash) and correct track count. ffmpeg/ffprobe may be used **only as external test oracles**, never linked or shipped.
- **Benchmarks** (`cargo xtask bench`): test-game frame times with Klip off vs on; RAM and CPU; save latency. State the machine and settings.
- **Everything is a command:** every action (clip.save, settings.set, editor.trim, export.start …) is a command with an id, used by the UI, CLI and a local **control channel** (JSON lines over localhost, dev flag or opt-in). Drive the app through it and take UI screenshots to check UI work.
- **Real games:** you can't play. Write short manual test checklists for me, and never launch anti-cheat games without asking.

## 6. Working conventions

- Start every session by reading `plan/STATUS.md`, then the task in `plan/execution-plan.md`, then the relevant crate README.
- Loop: pick the task → write acceptance tests first → implement → `cargo xtask ci` (fmt, clippy `-D warnings`, tests, `cargo deny`, layer check, asset/licence audit, front-end lint/tests) → drive and look for UI work → update the crate README, `ROADMAP.md` and `CHANGELOG.md` → commit as `M<n>.<k>: …` → next task.
- Only commit green states. Don't push or publish without asking.
- Layering: capture/encode/buffer/mux crates have no UI dependencies; the Tauri app depends on them, never the reverse. Enforce with `cargo xtask layers`.
- Parallel agents (only if I ask): one git worktree, one `CARGO_TARGET_DIR` and one crate owner per agent, branches prefixed `opus/`, every change logged in `CHANGELOG.md` with who and why.
- `ROADMAP.md` keeps an honest assessment: what works, what's approximate, known bugs, measured vs estimated.
- Keep usage lean: targeted reads, trimmed output, one build/test run per batch of changes, a handoff note at the end of each milestone.
