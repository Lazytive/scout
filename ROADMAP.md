# Roadmap and honest status

Last updated: 2026-10-10 (M9 done).

| | Milestone | State |
|---|---|---|
| M0 | Repo, rules, gates | **done** |
| M1 | Recorder CLI (Windows, NVENC) | **done**: acceptance passes |
| M2 | Audio (game/per-app/mic tracks, A/V sync ≤ 1 frame) | **done**: acceptance passes |
| M3 | All GPUs + fallbacks, HDR tone-mapping | **done** (AMD/Intel unverified, see below) |
| M4 | Linux (portal + PipeWire, VA-API) | **done on KDE Wayland + NVIDIA** (`cargo xtask e2e` green); GNOME, X11, AMD/Intel unverified |
| M5 | Full-session recording + bookmarks | **done**: acceptance passes on Windows (Linux not yet run) |
| M6 | Desktop app shell (Tauri) | **done on Windows**: tray app, first-run setup, settings, hotkeys, game detection + profiles, presets, storage cap, overlay, themes, `cargo xtask bench`; recorder in a child process (M6.12: memory back after a game); Linux paths written, not yet run |
| M7 | Library | **done on Windows**: grid with day headings, hover-scrub strips, search, filters (game, date, starred, tag), star, rename, tags, delete to Recycle Bin, open / show in folder; info and thumbnails rebuilt from the folder; Linux not yet run |
| M8 | Editor, Normal mode + export presets | **done on Windows** (ADR 0017 accepted): `klip-edit` model + text, `klip-export` pipeline, engine commands, the Editor workspace, e2e `m8_editor.rs`; Linux not yet run |
| M9 | Editor, Advanced mode (incl. montage) | **done on Windows** (ADR 0018 accepted): schema v2, the composition renderer (montages, transitions, zoom, stickers, colour, ducking, Custom export; 10-minute montage 10.3x realtime, < 800 MB), `editor.frame`/montage projects, the Advanced workspace with a canvas preview and Web Audio sound (≥ 39.9 dB vs the export), e2e `m9_montage.rs`; human checklist steps 2–10 open; Linux not yet run |
| M10 | Themes v1 | |
| M11 | Sharing | |
| M12 | Auto-clip via official integrations | |
| M13 | Release 0.1 | |

## What works (measured)

Machine: Windows 11 Home 26200, Intel i7-14700KF, 64 GB RAM, NVIDIA RTX 4080 SUPER
(driver 32.0.16.1742), NVMe SSD, GStreamer 1.28.6 MSVC, Rust 1.99. Settings: H.264
`nvd3d11h264enc`, 20 Mbit/s VBR (max 30), GOP 60, no B-frames, capture of a 1280x720
window at 60 fps, test game at 120 fps; audio AAC-LC 160 kbit/s 48 kHz stereo. Numbers
from `cargo xtask e2e` on 2026-10-09. During these runs another game was using the GPU
(up to 99 %), which slowed the test game at times; capture still kept every frame.

Video (M1, re-measured after the M2 timing changes):
- `klip-cli record --buffer 120s` + `klip-cli clip --last 30s`: 30.56 s clip, 1833
  frames, 59.98 fps, frame intervals 16.67 ms at p1–p99 (max 20.1 ms), every barcode
  readable, none repeated or out of order, no gap longer than one capture tick.
- Save time: 83 ms for a 30 s clip with audio, of which 74 ms is waiting for audio to
  reach the clip's end (audio arrives ~50–100 ms after video); the snapshot + write +
  fsync + rename part is ~9 ms (M1: 9.0 ms video-only). CLI round trip incl. process
  start ~60–70 ms more.
- Synthetic exactness test (600 known frames → NVENC → ring → MP4 → ffmpeg decode):
  600/600 frames back, in order; a 2 s cut is exactly 120 frames starting on a keyframe.
- Encoders (M3, `every_working_encoder_round_trips_exactly`, real-time feed, GPU at
  84–99 % from another game): all 14 installed encoders give back every frame exactly
  and cut on keyframes: NVENC D3D11 + CUDA (H.264/H.265/AV1), D3D12 H.264 (both DXGI
  adapters), NVIDIA MF H.264/HEVC (both adapters), Microsoft software MF H.264/HEVC.
- Encoder choice: ranked list for the monitor's GPU (here `nvd3d11h264enc`,
  `d3d12h264enc`, `mfh264enc`, …); isolated probes take 0.9–1.7 s each, once per setup
  (cached); a recorder whose first encoder can't start runs with the next one
  (missing AMF → NVENC: started in 206–227 ms).
- Own WGC capture (M3.4) with the test game at 1280x720, 60 fps capture, GPU at 83–99 %
  from another game: 31 s clip, 1841 frames (59.25 fps), every barcode readable, none
  out of order, repeats within what the game's own stalls explain; monitor capture works
  (fallback test). Frames stay on the GPU (shared D3D11 device).
- HDR (M3.5, `tests/m3_hdr.rs`, monitor in HDR mode, SDR white 140 nits, peak 648 nits):
  an SDR game comes out exact (patches 16/64/128/192/235/255 back unchanged, checker
  59/89 for 60/90); an HDR game with paper white = SDR white keeps its SDR parts, and its
  80/160/320-nit patches land at 199/236/249 while 640 nits (the display's peak) and
  brighter reach 255; the measured scene peak is 648 nits. Same on the 120/400-nit
  monitor: 211/241/252.
- Fallbacks (M3.6, `tests/m3_fallback.rs`, test game borderless on the secondary
  monitor): the game stops drawing for 5 s → window capture switches to monitor capture
  after 3 s without frames (9.3 s into the recording), restart ~0.25 s, the clip holds
  frames from before and after the switch (counters 111–684), all readable, in order.
  DXGI monitor capture of the game: barcodes readable, gray patches exact on the HDR
  desktop.
- Platform: both monitors here report HDR on (SDR white 120 / 140 nits, peak 400 / 648
  nits), the adapter that drives them, and the virtual-display copy of the GPU.

Audio (M2):
- A/V sync with the test game (beep at every flash, game audio by process loopback):
  30/30 beeps matched in a 30 s clip; audio stamped 0.2–1.0 ms after the audio engine
  played each beep (reference: the game's own WASAPI loopback tap), video 5–10 ms after
  the GPU finished each frame (reference: the game's frame log); net A/V −4.7 to −9.1 ms
  over 4 runs (audio slightly early), spread within one run < 2 ms. Target: ≤ 1 frame
  (16.7 ms).
- Process-loopback lag measured live: 11.5–20.5 ms across sessions, converging within
  the first ~2–4 s (status `audio.process_lag`).
- System-audio path (diagnostic): audio 0.7 ms after the tap.
- AAC (`mfaacenc`): beeps within 0.01 ms of their input time after encode → MP4 → ffmpeg
  decode; encoding 220x real time.
- Clip layout: one input → one named track (e.g. "Game"); with `--mic` → Mix (default,
  played by players), Game, Microphone (disabled alternates for editors).
- Excluded app (`--audio system --exclude-app klip-testgame.exe`): 0 of 10 beeps in the
  clip.
- WASAPI capture: process and system loopback deliver continuous 10 ms packets with QPC
  stamps; a device that can't be opened records silence (unit tests).
- Recorder CPU with game audio + timing reference + NVENC 720p60: 6.2 % of one core.
- Replay ring, MP4 writer (now with track names, enabled flags, alternate groups),
  control channel, frame grid, drift aligner, lag estimator, app-exclusion planner: unit
  and property tests.

### Linux (M4, so far)

Machine: same PC on Bazzite 44 (KDE Plasma 6.7.5 Wayland), NVIDIA driver 615.71,
GStreamer 1.28.7, PipeWire 1.6.9; two 3840x2160 monitors at scale 1.7. `cargo xtask ci`
green (built in a Fedora 44 distrobox).
- Encoders: `nvh264enc`, `nvh265enc`, `nvav1enc`, `openh264enc` round-trip 240 barcoded
  frames exactly (`every_working_encoder_round_trips_exactly`); 600-frame ring + MP4
  test passes.
- M1 acceptance through the portal (test game fullscreen 3840x2160 @ 120 fps on the
  second monitor, 60 fps capture scaled to 1280x720, NVENC H.264): 15.27 s clip, 60.00
  fps, every barcode readable, no repeats or reordering, frame interval p99 17.2 ms; save
  4 ms.
- Recorder CPU 4K stream → 1080p60: 31 % of one core, RSS 457 MB (no audio).
- Audio (PipeWire): per-app capture by linking the app's streams: a 1 kHz tone into a
  private null sink → included 0.354 RMS (full level), excluded 0.011, sink monitor 0.354.
  AAC: `fdkaacenc` 2048-sample delay and `avenc_aac` 1024, compensated to < 0.01 ms.
- `cargo xtask e2e` green on Linux (146 s, 2026-10-09; Firefox paused): M1 clip 30.2 s,
  60.02 fps, no unreadable/repeated/reordered frames, save 30 ms; all four encoders
  round-trip; encoder fallback starts in 214 ms; per-app capture: tone included 0.354
  RMS, excluded 0.000, sink monitor 0.354; Mix + Game + Microphone tracks.
- A/V (`m2_audio.rs`, idle desktop, video stamped on arrival minus KWin's measured 24 ms
  screencast latency, ADR 0012): game audio (`--game`, per-app) in four runs: A/V
  +0.07, −8.84, −11.91 ms (each run's beeps within ±0.9 ms), system audio −5.1 ms. The
  spread between runs (video +1 to +12 ms after the GPU) is KWin's scheduling; all
  within one 60 fps frame. Per-app streams are one graph cycle early and are shifted by
  one buffer; the lag estimator then measures ±0.13 ms.
- Hotkey (GlobalShortcuts portal, KDE): Alt+F10 bound after one confirmation dialog; a
  press saved a valid 5.3 s clip in 4 ms (manual run; key presses can't be automated
  on Wayland without extra permissions).

### Full-session recording (M5)

Same machine and settings as above (Windows, `nvd3d11h264enc`, 1280x720 window at 60
fps, game audio AAC), `cargo xtask e2e` on 2026-10-09, another game using the GPU:
- **Killed recorder** (`m5_session.rs`): `record --session`, two bookmarks, the recorder
  killed after 10 s (TerminateProcess). The file left on disk played in ffmpeg as it was:
  H.264 + AAC, 540 frames in order, none unreadable, repeats only where the test game
  itself stalled. `klip-cli finalize` turned it into a regular MP4 (`ftyp, free, (free,
  mdat)…, moov`, no `moof`) with the same 540 frames and both bookmarks as chapters at
  4.087 s and 7.126 s (the bookmark command answered 4086 and 7126 ms).
- **Normal stop**: `session start` → 12 s → `session stop`: regular MP4, 12.108 s video,
  audio ends 9 ms after it (stopping cuts both at the video's end), 710 frames in
  order, A/V −3.1 ms (11/11 beeps; −5.3 ms in an earlier run before the stop change).
- **Exactness** (`synthetic_session.rs`: 600 known frames → NVENC → ring → recording →
  ffmpeg): a copy taken mid-recording (what a kill leaves) decodes as frames 0–299 with
  none missing; finalising that copy keeps exactly those 300; the stopped recording holds
  600/600 frames in order at 60 fps with the bookmark chapter at 2.500 s.
- Unit tests: truncation at every byte of the last fragments, every intermediate state
  of an interrupted finalise (gives the identical file), 3000 mutated files (no panic),
  rollover to a new file when the video description changes, queue overflow, recovery
  of leftover recordings (incl. empty and garbage sidecars).

### Engine and game detection (M6.4)

Same machine, `cargo xtask e2e` / `m6_daemon.rs` on 2026-10-09 (GPU mostly idle), the
test game on the user's game list, settings: Custom 1280x720 at 60 fps, 20 Mbit/s,
game audio, buffer 30 s, clip hotkey F24 (10 s):
- Detection poll (process + window snapshot + rules): median 3.6 ms, max 4.7 ms (one
  poll every 2 s). A user-listed game was noticed 0.8 s after its window appeared; the
  buffer stopped 2.0 s after the game exited.
- The hotkey (pressed with `SendInput`) saved a 10.03 s clip into `clips/Klip Test Game/`
  in 88 ms (82 ms of it waiting for audio): 600 frames, none repeated, unreadable or out
  of order; H.264 + the game's audio track.
- Idle cost of the engine with no game: 0.00 % of one core over 6 s, 22 MB private
  memory (57 MB resident). After a game it stayed at 0.26 % CPU but **106 MB private**
  (111 MB resident) until M6.12; since the recorder runs in a child process (ADR 0016)
  it is back where it was: 23.1 MB private before and 23.1 MB after the game (release
  build, 2026-10-09), and no Klip worker process is left 22 s after the game (the
  library's media worker makes the clips' strips once the game is over, then stops).
- A recorder process killed mid-recording (M6.12 e2e): a new one runs 1.4–1.5 s later,
  and the recording it left is finalised by recovery 17.5 s after the kill (it waits
  until nothing could still be writing it) and plays as H.264 with what was written.

### What Klip costs a game (M6.10, `cargo xtask bench`)

Same machine (i7-14700KF, RTX 4080 SUPER, driver 32.0.16.1742), GPU otherwise idle,
2026-10-09. Scene: the test game at 1920x1080 in an off-screen window, a shader load
calibrated to ~240 fps uncapped. Klip: the engine (`klip-cli daemon`) on Balanced
(nvd3d11h264enc 1080p60 20 Mbit/s, game audio, 2 min buffer). Each scenario is one long
game run with the replay buffer switched off and on (8 and 4 interleaved pairs, each
"on" stretch against the "off" stretch just before it), because GPU boost and background
use drift by several percent between separate runs.
- **Capped at 144 fps (frame cap / vsync, the usual case): no cost.** 144.0 fps with
  and without Klip; frame-time p99 7.58 → 7.66 ms.
- **Uncapped, GPU-bound (worst case): 2.9 % ± 1.0 % (95 %) fewer frames** (median 3.4 %,
  238.6 → 231.8 fps; p99 4.78 → 4.83 ms). Earlier runs of intermediate code measured
  8–10 % (with a frame-skipping experiment since removed, and possibly more background GPU
  use): the number moves with what else uses the GPU; treat ~3 % as this machine's value,
  not a guarantee. The brief's "mid-range GPU" is not tested.
- Engine while buffering: 6.0 % of one CPU core (0.2 % of 28), 173 MB resident, 245 MB
  private (includes the replay buffer).
- 30 s clip saves: 100–107 ms (each includes ~80–90 ms waiting for audio to catch up).
- Idle, no game: 0.00 % CPU, 89 MB private. **After a game: 0.16 % CPU but 379 MB
  private** (285 MB resident): encoder, decoder (thumbnails) and GPU driver memory stay
  with the process once used. Fixed in M6.12, below. (The 89 MB was sampled during the
  engine's first seconds: it holds ~100 MB more for ~8 s after start, then 22 MB.)
- Capture asks Windows for at most 2x the recording rate (`MinUpdateInterval`, Windows 11
  24H2+): no measurable change in this scene, but it spares the compositor half the
  copies at 240 Hz. Finding along the way: GStreamer's WGC source and Klip's own capture
  cost about the same here, and HDR (FP16) capture costs no more than 8-bit.

### Recorder in a child process (M6.12, `cargo xtask bench` before/after)

Same machine and scene as above, 2026-10-09, both runs back to back with a YouTube video
playing on the other monitor (the human's choice; it moves the fps numbers a little, not
the memory). Before = `main` at M7 (`b24f3f9`, recorder in the engine); after = M6.12
(recorder in `klip-cli --klip-recorder`, overlay poster from a one-shot media worker).
"Engine" after = the engine process plus its recorder process.

| | before | after |
|---|---|---|
| **Engine private memory after the game exited** | **381 MB** (292 MB resident), 0.31 % CPU | **22 MB** (58 MB resident), 0.31 % CPU |
| Engine idle before any game (after the start-up transient, ~8 s) | 23.7 MB (start-up trace) | 22 MB (21.8 MB in the trace) |
| While buffering (Balanced, 2 min) | 242 MB private, 178 MB resident, 5.8 % of one core | 264 MB private (engine process 22 MB), 241 MB resident, 7.0 % of one core |
| 30 s clip saves | 75–104 ms | 85–94 ms; 86–95 ms end to end over the control channel |
| Overlay picture after a save | 33 ms (1.18 s for the first save of a session; decoder kept warm in the engine) | 127–132 ms (one-shot media worker) |
| Game fps cost, uncapped | 3.86 % ± 1.00 (median 3.53 %) | 3.73 % ± 0.46 (median 4.05 %) |
| Game fps cost, capped at 144 | none | none |

- Recorder start, from switching the buffer on to `recorder_started`: 1.58–2.04 s
  (median 1.75 s). That includes the recorder process's Media Foundation start-up
  (0.72–0.77 s measured on its own), which the in-process recorder had paid once when
  the engine started; the in-process start time itself wasn't measured.
- A poster or strip in a fresh media worker was slow for a reason unrelated to process
  start: `klip_probe::init` started Media Foundation (0.72–0.77 s) and the GPU decoder
  took 0.3 s to start. Thumbnails now only initialise GStreamer (33–36 ms) and the
  media worker prefers software decoders for its few key frames (ADR 0016): poster
  1.27–1.36 s → 132–271 ms, 16-tile strip 1.45–1.59 s → 437–443 ms (30 s 1080p60 H.264
  test clip).
- `bench-*.json` keeps the engine process and the total apart
  (`engine_process_private_bytes`, `engine_private_bytes`) and now records
  `round_trip_ms`, `thumbnail_after_ms` and `recorder_start_ms`.

### Library (M7)

Same machine, `cargo xtask e2e` / `m7_library.rs` on 2026-10-09 (the test game at
1280x720 off-screen, clips of 8 s at 720p60 H.264 12 Mbit/s; another game may have been
using the GPU):
- A card that comes into view gets its hover-scrub strip in 1.5–1.6 s, including
  starting the media worker process (GStreamer start-up + 16 key-frame seeks and
  decodes); while a game is recorded only such visible cards are done. Since M6.12
  (no Media Foundation start-up in the worker, software decoding of key frames):
  0.25 s in the same test.
- Background strips start 0.12 s after the game ends (the engine wakes the thumbnail
  thread when the followed game stops).
- A strip of the test game is 313–322 KB (16 x 240x135, JPEG quality 75; the barcoded
  test pattern compresses badly, real games are not measured yet). All 16 frames differ.
- Poster for the library = the overlay's picture of the just-saved clip (no extra work).
- Clip info (duration, size, fps, codec, bookmarks) read from the `moov` only matched
  the recorder: 7–9 s, 1280x720, H.264, 60 fps ± 3.

### Text rendering (M8.2, `klip-edit`)

`cargo test -p klip-edit --release --test text_render -- --ignored timing --nocapture`
on 2026-10-10, i7-14700KF, one thread, the two-line sample of the golden test (18 + 8
characters, an em dash and an emoji box), one layer rendered and PNG-encoded:

| Preset | 1920x1080 | 1080x1920 (3 lines) | 3840x2160 |
|---|---|---|---|
| bold (outline + shadow) | 13.7 ms, 44 KB | 53.0 ms, 85 KB | 67.7 ms, 99 KB |
| pill | 2.7 ms, 19 KB | 6.5 ms, 33 KB | 8.2 ms, 39 KB |
| caption (outline + shadow) | 6.7 ms, 29 KB | 21.9 ms, 55 KB | 27.1 ms, 60 KB |
| title | 9.3 ms, 43 KB | 14.7 ms, 60 KB | 32.4 ms, 89 KB |

The outline (a disc splat from every edge pixel) dominates; the tall 9:16 `bold` is
the worst case of the presets (173 px glyphs, 21 px outline). Debug builds are about
10x slower (the 96-case proptests take 6 s). Nothing is cached yet: M8.4 writes the
PNGs under `.klip/render/<cache_key>.png`.

### Export (M8.3, `klip-export`)

`cargo test -p klip-export --release --test export -- --ignored --nocapture` on
2026-10-10, i7-14700KF + NVIDIA (NVENC H.264, D3D11 mode), software decoding
(`d3d12h264dec` was free to be picked in-process; the worker demotes hardware decoders),
a synthetic barcoded 1280x720@60 clip with two AAC tracks, ffmpeg as the oracle:

| Export | Frames | Time | Output |
|---|---|---|---|
| Original: trim + cut + 2x + 0.5x, 8 s out of 20 s | 480 | 1.0–1.7 s | every frame = the plan's source frame; 60 fps; one Mix track; faststart |
| Original with a text cue (5 s) | 300 | ~1 s | overlay region PSNR 45.4 dB vs the bitmap over the source frame; 9.4 dB without it |
| Discord 5 / 12 / 25 / 40 / 60 s at 10 MB | 300–3600 | 0.7 / 1.4 / 5.4 (two passes, forced) / 4.4 / 4.4 s | 20 / 17 / 35 / 57 / 53 % of the limit; 60 s went 720p30 at 1125 kbit/s |
| Cancel at frame 60 of 600 | – | < 1 s | no file, no `.part` |
| TikTok from a tall (9:16) crop at `crop_x` 0 / 0.5 / 1 | 240 each | ~0.5 s | 404x720; the kept columns match the source (PSNR 42–44 dB), a wrong offset is ≥ 3 dB worse |
| Discord on a 1920x1080 clip at a 400 kB limit | 151 | ~0.5 s | steps down to 852x480 at 30 fps, every frame = the plan's source frame |
| Bookmarks through trim + cut + 2x/0.5x | – | – | 3 of 4 survive as chapters at the mapped output times (1 ms), the cut one is gone |
| Beeps through the same cuts, and 0.5x / 2x whole-clip | – | – | onsets at the mapped times within 10 ms (1x: 1 ms; 0.5x: a stretched attack reads ~35 ms early) |
| Through the daemon (e2e `m8_editor.rs`): a 10 s clip the daemon recorded of the test game, trim + a 2x piece + a caption, Discord | 390 | encode 1.0 s; 5.2 s wall incl. starting the media worker and the probe | 3.3 MB; 390/390 frames = the plan's source frame, 0 unreadable; listed in the Library beside the clip |

So an export runs at roughly 300–500 output frames per second at 720p on this machine
(decode + crop/scale on the CPU, NVENC encode). 1080p and 1440p sources are not measured
yet; the Library's real clips are the M8.6 job.

### Compositing spike (M9.1, `klip-export`)

`cargo test -p klip-export --release --test export -- --ignored diag_compositor_spike
--nocapture` on 2026-10-10, i7-14700KF + NVIDIA (NVENC H.264), software decoding of two
1920x1080@60 10 s clips, the second zoomed 1x → 2x by a per-frame keyframe (pad
width/height/xpos/ypos) and crossfaded in over 1 s (pad alpha), into the encode chain:

| Compositor | 600 frames in | Realtime |
|---|---|---|
| `compositor` (system memory, NV12) | 2.15 s | 4.6x |
| `d3d11compositor` (D3D11 memory) | 1.18 s | 8.4x |

Both over the 2x bar in ADR 0018, so Advanced mode extends `klip-export`.

### Composition renderer (M9.3, `klip-export`)

Measured 2026-10-10, i7-14700KF + NVIDIA (NVENC H.264), 1280x720@60 barcoded clips made
by the tests, hardware decoding (in-process test), the user's game possibly running:

| Test (`tests/export.rs`, `--ignored`) | Result |
|---|---|
| `long_montage_stays_under_a_gigabyte` (release): 10 min, 20 layers (main, nine library clips with 1 s crossfades, five with a 1x → 1.5x zoom, ten music pieces) | 36 000 frames in 58 s = **10.3x realtime**, peak working set **798 MB** (the test process: 116 MB before) |
| `montage_crossfade_maps_every_frame` (debug): two clips, a 500 ms crossfade | 211 frames on the plan, 30 blended, **0 wrong** |
| `zoom_shows_the_centre_and_colour_grades` | 2x zoom vs the scaled source centre **42.5 dB** (vs the unzoomed frame 14.0 dB); brightness +0.2 → **+49** levels (ideal 51) |
| `music_ducks_under_a_voice` | music **−11.91 dB** under a voice layer (setting −12); unducked ±0.5 dB |
| `sticker_sits_where_the_plan_puts_it` | opaque half 255, 50 % half 163.9 (ideal 164.8) |
| `custom_preset_honours_its_numbers` | 640x640 box → 640x360, 30 fps, h264; 685 kbit/s at a 1500 target, 354 at 150 (simple frames undershoot) |

Normal-mode exports go through the same renderer; the six M8.3 export tests pass on it.
Re-run after M9.4's refactor (2026-10-10, same machine, the user's PC in use): all 13
export tests pass; the 10-minute montage at **9.2x** realtime, **785 MB** peak.

### Editor frames and montage projects (M9.4)

Measured 2026-10-10, same machine:

| Test | Result |
|---|---|
| `frames_match_the_export` (`klip-export`, debug, hardware decoding in-process): the crossfade montage, `frame::frame` at 1280 wide vs the exported frame at the same time, ten times before/inside/after the crossfade | barcodes **all as planned**; PSNR vs the export **48.3–51.1 dB**; **49–220 ms** a frame (JPEG 20–78 KB) |
| `frames_of_a_montage` (`klip-engine`, `editor.frame` end to end, in-process, 640x360 clips) | first frame **431 ms** (GStreamer start-up), then **34–56 ms**; a tall unsaved project gives a 180x320 frame |
| `montage_missing_clips_history_and_rename` (`klip-engine`, not hardware) | append, Custom export job, history coalescing and restore, rename followed, deleted clip reported `missing` and kept, export refused by name |

### The Advanced workspace's preview (M9.5)

Measured 2026-10-10, same machine, the real app (debug build) off-screen, driven through
WebView2's remote-debugging port; two 12 s barcoded 1280x720 H.264 clips with Game and
Microphone tracks; the project: the main clip cut to 0–6 s, the second clip after it
with a zoom ramp (1 → 1.6, x +0.1), the main clip again crossfading in over 1 s, a
half-transparent PNG sticker zoomed 0.8 and moved, the grade brightness +0.05 /
contrast 1.1 / saturation 1.3.

| Check | Result |
|---|---|
| Canvas preview vs `editor.frame` (the export's own drawing step, JPEG) at the same time and size (1280x720), RGB PSNR | 3 s (main only, graded) **66.8 dB**; 7.5 s (mid-zoom) **41.6 dB**; 9.5 s (zoomed) **41.5 dB**; 11.5 s (crossfade + sticker) **39.9 dB**; 14 s (main + sticker) **44.0 dB**: all ≥ 35 dB (the bar). Each check (frame, seek, draw) 85–193 ms. |
| Playback, Advanced: picture vs the clip's split sound tracks after 2 s | video 10.960 s, Game and Microphone 11.000 s of the source (within one 40 ms sample), video muted |
| Playback, Normal mode (the Sound sliders' bug): picture vs tracks after 2 s | video 1.954 s, both tracks 1.991 s, playing; video muted, the tracks carry the sound through the gains |
| `editor.audio` split (`klip-export` hardware test): a 4 s two-track clip | 6–28 ms; every beep of each file where the clip's track has it (±5 ms) |
| `editor.audio` through the engine (`klip-engine`, not hardware) | names `Game`/`Microphone`, one audio track per file, length 2.0 s as made, not remade when asked again, remade when the clip changes, follows a rename |
| Shared math, Rust vs vitest | `placement.json` (fit, fitted, place, visible, colour matrix) and `compose.json` (three projects: Normal cut, montage with crossfade/dip/ramp/sticker/sound/music/markers, edge cases): identical |

### A montage of recorded clips (M9.6, e2e `m9_montage.rs`)

Measured 2026-10-10, same machine (NVENC H.264, the user's PC in use), through the
daemon: the test game at 1280x720 recorded for 16 s, two 6 s clips saved 8 s apart
(400 frames each, ~2.26 MB). The montage: the first clip trimmed to 1–5 s (Normal
mode), the second added with `editor.montage` and split in two (0–2.5 s, then 3–5.5 s
crossfading in over 500 ms and zoomed 2x from 1.5 s in), Custom export 1280x720@60
H.264 12 Mbit/s.

| Check | Result |
|---|---|
| Export | 510 frames (8.5 s) in **1.2 s** encode, **2.2 s** wall incl. starting the media worker; 4.4 MB |
| Frames on the plan (`compose_with`, barcodes via ffmpeg) | **420/420** plain frames = the plan's source frame (0 wrong, 0 unreadable) across both clips; **30** crossfade frames, **60** zoomed, as planned |
| Crossfade, middle frame (alpha 0.5) vs the mix of the two source frames | **43.6 dB** (each side alone 20.9 / 21.1 dB) |
| Zoom, a 2x frame vs the source frame the plan names, doubled about the centre | **43.7 dB** (the unzoomed frame 14.9 dB) |
| Library | the export listed beside its clips, 8.5 s, H.264, one audio track; the montage follows a rename of its second clip |

### Advanced mode in FilmCraft's layout (M9.7)

Measured 2026-10-11 in the real app (debug build, off-screen, driven over CDP, the
user's own Klip running beside it; scratch library: two 12 s barcoded test clips with two
AAC tracks each, a 6 s WAV as music). Project: Clutch as Main, Second on a montage track
from 12 s crossfading in **from below** over 1 s, a second montage clip at 15 s, music
at 0 s.

| Check | Result |
|---|---|
| Preview canvas vs `editor.frame` (export renderer), 1280x720 | **42.4 dB** at 10.5 s, **41.9 dB** at 11.5 s (mid-fade from below), 42.4 / 42.7 dB at 12.5 / 16 s |
| Export of a crossfade from below (GPU encoder, `crossfade_from_below_maps_every_frame`) | 241 frames: every plain frame on the plan, **30** blended (500 ms at 60 fps), Main before, the clip from 2 s, Main again at 3.5 s |
| Timeline | sprite thumbnails on video clips (14 on Main, 9 on the montage), waveforms on Main sound and the music, the from-below block on the montage's first clip |
| Meters while playing | both sides move (left peaks about −1 dB, right about −5 dB with the left-heavy test music) |
| Drag a Project card onto the empty track | a new track, the clip at the dropped time (1.784 s, where the pointer was), one undo step |
| Gestures (mock, headless Edge with trusted input) | a click selects without moving; 2 px does nothing; a move to another track ends cleanly; transition ends follow the pointer |

### FilmCraft feel (M9.8)

Measured 2026-10-11. Mock = the UI against the mock engine in headless Edge with trusted
CDP input; real app = debug build, off-screen, its own settings and WebView2 profile,
the user's Klip running beside it.

| Check | Result |
|---|---|
| Zoom through edits (mock) | 32.69 px/s before and after two edits that took the edit from 30 s to 50 s |
| Selection (mock) | Shift-click → 2 clips; dragging one moved both by 1989 ms; a plain click on one collapses to it; Ctrl-click twice adds and takes out; a marquee over 3 clips selects them; Shift+Delete removed them and moved the clip after them from 30 s to 16 s (their 14 s); Ctrl+Z restores |
| Razor and Ctrl+K (mock) | C, click at 40 % of a 5 s clip → cut at 2.0 s into it; Shift-click on Main → Main and the montage clip under it cut at 6 s; Ctrl+K at 16 s with nothing selected → the clip there and Main cut; the dashed cut line on all 3 rows with Shift |
| Rolling a cut (mock) | a dip's middle dragged 33 px → the cut moved 1010 ms: the first clip 1010 ms longer, the next 1010 ms shorter (source in point +1010 ms) |
| Dip to black on V2 over Main, export (`dip_on_a_montage_track_goes_through_black`) | 241 frames: 60 in the dip (1 s), every other frame on the plan; mean luma **70.9** before, **35.3** half way, **0.0** at the cut (Main used to show there) |
| The same in the preview | mock canvas mean 66.6 → 33.1 → **0** → 59.5; real app vs `editor.frame` at 640 px: 38.4 dB (plain), 43.5 dB (mid-dip), **identical** at the cut (both black), 41.9 / 40.0 dB after |
| Browser UI (real app, trusted keys) | Ctrl+P, Ctrl+S, Ctrl+F, F5, Ctrl+Shift+I and a right-click on the page: default cancelled; Ctrl+Z still reaches Klip; a right-click in a text field keeps its menu |

## Approximate / not done yet

- **The FilmCraft-style Advanced mode (M9.7, M9.8)**: waveforms decode the whole sound
  file in the page (files over 96 MB get none; a long main clip without separate tracks
  decodes its proxy); thumbnails and waveforms ignore speed ramps inside a clip; a
  stopwatch turned on with only a key at the start stays on for this session only; a
  crossfade still pulls the next clip earlier and leaves clips after it where they
  were (as before M9.7). Tools are two toggles in the timeline header, not a strip (the
  user's choice). Dragging a transition's middle rolls the cut instead of sliding the
  transition off-centre (Klip has no media past a clip's ends). A selection moves in
  time only (one clip may still change track). On a hidden video track (the eye) the
  picture still shows: only its sound goes (as since M9.5; the icon says "hide").

- **The Advanced preview (M9.5)**:
  - Heard by the human 2026-10-10: the preview's sound and Normal mode's Game/Mic
    sliders work (checklist step 1). Not heard yet: volume keys, muted tracks, music
    ducking under voices (unit tests, element timing and the output meter only). Ducking in the preview follows the voice through an analyser
    every frame (≈ 16 ms), the export per 10 ms block: close, not identical.
  - Speeds below 0.5x or above 4x may be silent in the preview (Chromium mutes media
    outside its pitch-keeping range); the export is right.
  - Reverse shuttle (J) steps back by seeking, so it is choppy; forward 2x/4x plays.
  - A sticker or sound added in a session is shown through the file dialog's own
    permission; after reopening, `editor.files` allows it again (picked files with an
    odd extension are refused by the engine and show as missing).
  - Several HEVC montage clips: their proxies are made one at a time (the second one
    starts when the first is done; until then its strip tile shows).
  - Dip transition on an upper track fades that track out and in again, so the tracks
    below show through (black only on the lowest track). Decided: that is the
    behaviour; the inspector says so.
- **Editor frames and montages (M9.4)**:
  - `editor.frame` opens every clip on screen per call (no cache): fine for a paused
    preview and parity checks, too slow to scrub with (the canvas preview, M9.5, is for
    that). In the app it runs in a media worker of its own (not measured in the app yet).
  - Event markers come from chapters with a title other than `Bookmark <n>`; Klip writes
    none yet (auto-clip integrations are M10+), so in practice they are bookmarks.
  - Deleting a montage's first clip deletes the montage (its project lives under that
    clip). Deleting another clip leaves it `missing` in the montage; the export refuses
    with the clip's name until it is removed from the edit.
  - History snapshots: at most one a minute while saving, 30 kept per project.

- **Renderer (M9.3)**:
  - Zoom and position snap to whole pixels (the visible source rectangle is whole source
    pixels, the placement whole output pixels): a slow zoom can wobble by up to a pixel.
  - A dip transition on an upper track fades that track out over the tracks below it,
    not to black (`compose` turns transitions into opacity ramps).
  - Composing is on the CPU (`GstVideoConverter` + own blending); `d3d11compositor` is a
    later optimisation. A zoom animation builds a new converter every frame.
  - Ducking listens to microphone tracks and sound/voice-over layers only; game audio
    never ducks the music (by design, ADR 0018).
  - Not run yet: AV1/HEVC Custom exports, Linux. Montages of recorded clips: e2e
    `m9_montage.rs` (test-game clips); the human's own clips are checklist step 10.

- **Export (M8.3)**:
  - Discord's first pass lands well under the limit (17–57 % on the barcode clip: VBR
    encoders don't spend the whole budget on easy pictures), so the second pass is rare;
    it is exercised by a test knob (`pretend_overshoot`), not by a clip that really
    overshoots. A pass that aims closer (e.g. 0.97 and a re-encode when under 70 %)
    would give better quality; not done.
  - Scaling and text compositing run on the CPU (`videoconvertscale`, a BGRA copy per
    frame with text); decoding is software in the worker. `d3d11compositor`/GPU decode
    would be faster for 1440p/4K sources. The whole encoded output is held in memory
    before the MP4 is written (fine for clips; a 10-minute 4K export would be large).
  - No kerning or shaping in text (see M8.2).
  - The media worker's child-process export path (progress lines, cancel line) has unit
    tests for the protocol and the in-process path; the real child process is exercised
    with the engine commands in M8.4.
  - Exports are capped at 4 hours / an estimated 4 GB because the encoded output is
    held in memory until it is written (`plan/IMPROVEMENTS.md` G1: stream it).
- **Editor (M8.5)**:
  - The preview plays the clip itself for H.264 (the app's WebView plays H.264 + AAC)
    and an H.264 proxy for HEVC/AV1 clips, made by an export job on first open (a
    720p transcode: a few seconds; "Making a preview…" meanwhile, the still frames
    stand in). Seeks between pieces use `<video>.currentTime`, so a cut shows a short
    stall at the boundary; frame-exact is the export's job.
  - The text cues on the preview are the engine's bitmaps (identical pixels to the
    export) requested after each edit (250 ms debounce, one command per layer).
  - Trim handles move the ends of the kept range in source time; cuts in the middle
    come from a drag selection + Cut. There is no ripple-drag of cut edges yet.
  - Advanced mode is a disabled toggle (M9).
- **Text (M8.2)**: no text shaping. Latin, Greek and Cyrillic (what Inter covers) render
  well; Arabic and Hebrew come out as unjoined glyphs in logical (not visual) order,
  ligatures as separate letters, emoji as the font's "no glyph" box. A second font or a
  shaping crate (e.g. `cosmic-text`) is the fix if users ask for it.
- **Library (M7)**:
  - No playback inside Klip yet: Open hands the clip to the default player (the editor's
    preview is M8). Hover-scrub shows 16 key frames per clip, not every frame; long
    recordings get coarse strips.
  - The first listing of a big folder reads every clip's `moov` before the grid shows
    (cached afterwards in `.klip/index.json`); not measured beyond a few clips. The grid
    isn't virtualised: thousands of clips mean thousands of cards (images load lazily).
  - A separate recordings folder (`clips.recordings_folder`) is not part of the library.
  - Copy / drag out / Discord export are M11. Rename and delete of a clip that another
    program has open fail with the OS's message.
  - Linux: open (`xdg-open`) and show in folder (FileManager1 via `gdbus`) are written
    but not run; the media worker runs at normal priority there.

- **App shell (M6)**:
  - The window doesn't remember its size and position.
  - Changes to quality, audio or folders apply when the next game starts (the running
    buffer isn't restarted); "Allow local control" applies at the next start; voice apps
    that start after the game aren't added to its recording.
  - Per-game profiles cover buffer, quality, game sound and microphone (not custom
    numbers or voice apps).
  - Game detection is checked on synthetic snapshots and the test game; real store
    installs are on the human checklist. The engine follows the game's process, not its
    windows: a game that replaces its window isn't re-targeted (the capture supervisor
    falls back to its monitor, ADR 0011).
  - Overlay: Windows only (Linux gets the sound and notification; layer-shell later);
    its effect on VRR/G-Sync over a real game is unmeasured (human checklist or
    PresentMon); the quick menu takes the focus while open, so it isn't shown over an
    exclusive-fullscreen game.
  - While a game is recorded there are two Klip processes (the engine, ~22 MB private,
    and its recorder, ~240 MB with the 2 min Balanced buffer): 22 MB more than one
    process, 63 MB more resident. A game's recorder starts ~1.75 s after it is found
    (Media Foundation start-up included). A recorder process that hangs (alive but not
    answering) is reported on each command after a timeout, not restarted.
  - The cost numbers come from the benchmark scene on one high-end GPU; a real game and
    a mid-range GPU are not measured.
  - Linux: the app, the engine's portal path per game, process-only game detection and
    Proton paths are written but not yet run.

- **Full-session recording on Linux** is untested (same code; the gates and e2e run
  there next time). Recording and bookmark hotkeys work in the app and `klip-cli daemon`
  (Windows: RegisterHotKey; Linux: the portal), and in `klip-cli record --record-hotkey
  --bookmark-hotkey`.
- **Finished recordings are not faststart** (index at the end); sharing goes through the
  editor's export. Most video editors ignore `chpl` chapters, so bookmarks show in VLC,
  mpv and ffprobe now, and in Klip's editor later.
- A power cut can lose up to ~10 s of a recording (fsync interval); a killed process at
  most the fragment being written (≤ 1 s with our GOP).

- **Audio timing references are the test game's own logs.** Video is measured against
  the moment the GPU finished a frame (the compositor shows it at the next refresh, up to
  4.2 ms later on this 239 Hz monitor); audio against the engine's loopback tap. What
  reaches eyes and ears also includes display and audio-device latency, which differ per
  setup. No offset correction is applied.
- **Process-loopback lag** is measured against the default output's mix. If a game plays
  to a non-default device, or is silent, the default 16 ms is used (±3 ms error).
- **Long sessions:** drift correction repeats/drops single frames; not yet measured over
  hours (only seconds to minutes).
- **System audio without music apps:** when two or more music players play at once,
  apps that start playing later are not captured until the next recording.
- **Default device changes** during a recording are not followed (the device chosen at
  start keeps being captured; a removed device becomes silence and is retried).
- **Mic:** no gain, noise suppression or push-to-talk yet; off by default.
- **Save waits for audio** up to 250 ms (typically 50–100 ms).
- Static content gives ~1 fps (WGC only sends changed frames), so desktop capture is VFR.
- **AMF and QSV are unverified**: property names come from the GStreamer docs; no AMD or
  Intel hardware here. VA-API is M4. Hybrid-laptop ranking is by GPU vendor (two GPUs of
  one vendor can't be told apart) and untested on a real laptop.
- **HDR tone curve is a choice** (knee 0.8 in sRGB-encoded space, roll-off to the
  display's peak); it may need tuning by eye on real HDR games. The fallback source
  (`d3d11screencapturesrc`) still clips HDR content. A window that moves to a monitor
  with a different HDR state keeps its start monitor's settings.
- **A/V timing under GPU load**: frames are stamped when they reach Klip (bounded by
  WGC's time). Idle GPU: video +4.6 ms after the game's GPU-finish time, A/V −3.8 ms
  (old source: +4.2 to +5.9 ms). With another game holding the GPU at 86–100 %,
  composition itself runs late and every source drifts to +13–26 ms, so the ≤ 1 frame
  sync test can fail under such load.
- **NVENC and Media Foundation in one process** (ADR 0009): handled by starting MF first
  and ranking D3D12 before MF hardware, but a session on NVIDIA's MF encoder still breaks
  later NVENC sessions in that process (the start fallback then picks the next encoder).
- DXGI lists the RTX 4080 SUPER twice with different LUIDs (virtual display drivers from
  Parsec / Virtual Desktop); the copy has no outputs and ranks after the real one.
- **Exclusive fullscreen** switching is unit-tested only (an automatic test would take
  over a monitor and the focus); DXGI captures clip HDR highlights (no tone-map on that
  path).
- Not measured yet: FPS cost and RAM in a real game (the benchmark scene is measured,
  M6.10). The ring's cap is `(video + audio bitrate) x 1.5 x (buffer + 1 s)` = 457 MB for
  120 s at 20 Mbit/s + one AAC track; low-motion scenes fill much less (the bench scene
  used 7–18 MB for ~10–25 s).
- If the captured window closes while its process lives on, the recorder keeps the
  buffer (clips still save) but doesn't re-target a new window; when the process exits
  the engine stops the recorder (M6).
- **Linux** (M4, ADR 0012): video through the screen-cast portal and PipeWire audio work
  on KDE Wayland with NVENC; the hotkey works through the GlobalShortcuts portal; not
  yet: X11 and GNOME (M4.6), VA-API
  on real AMD/Intel hardware. **Video timing relies on a compositor latency measured on
  one machine** (KDE: 24 ms); it grows ~13 ms when another app keeps KWin busy, and other
  desktops are uncompensated until measured. Per-app audio needs PipeWire link
  permission and a shared PID namespace (fine natively; Flatpak needs checking in M13). Capture copies frames through system memory (31 % of one
  core at 4K → 1080p60); DMA-BUF is faster but shows stale frames on NVIDIA until
  explicit sync is handled. Windows/monitors can't be targeted by handle (Wayland).

## Manual checks for the human (M2)

Agents can't play games or listen. When convenient:
1. Record a game you play (`klip-cli record --window-title "<game>"`), save a clip, and
   check by eye/ear that gunshots or footsteps line up with the picture.
2. Same with `--mic` and `--app Discord.exe` during a voice call: the Mix track should
   contain game + voice + mic; open the clip in your editor (Premiere/Resolve/CapCut) and
   check it imports all audio tracks with their names.
3. Play music in Spotify while recording a monitor (`--audio system`): the clip must not
   contain the music.
4. Share a clip to Discord / a phone and check the sound plays (the Mix track).
Do not try this in anti-cheat games until the human decides (AGENTS.md rule 8); process
loopback is an OS API and doesn't touch the game, but that decision is theirs.

## Manual checks for the human (M3)

1. **HDR game** (Windows HDR on): record a game with HDR enabled in its settings, save a
   clip, and compare it with the game on screen: bright skies/lights should keep some
   detail instead of flat white, and dark/mid tones should look like the game. Say if
   highlights look too dull or too harsh (the curve can be tuned).
2. **Exclusive fullscreen**: a game set to "Fullscreen" (not borderless): start
   `klip-cli record --window-title "<game>"`, play a minute, save a clip; it must show
   the game, and `klip-cli status` → `video.switches` tells whether Klip switched to
   monitor or DXGI capture.
3. **Alt-tab** out of that game and back while recording: the clip should follow (some
   desktop frames while tabbed out are expected).
4. **AMD or Intel PC** (a friend's): `klip-cli probe` and a short recording; send the
   output of `klip-cli probe` and `klip-cli status`. AMF/QSV settings are untested.

## Manual checks for the human (M4, Linux)

1. **Stop sharing from the desktop**: while `klip-cli record` runs, stop the screen share
   from KDE's tray icon; `klip-cli status` must show `last_error` "screen sharing was
   stopped from the desktop" (only the desktop can end the session, so no automatic test).
2. **A real game on Linux**: `klip-cli record --game <its executable> --hotkey`, play,
   press the hotkey; the clip should have the game's sound only (Discord etc. left out)
   and look in sync. Steam games run under Proton show up as e.g. `GameName.exe` under
   `wine64-preloader`/`proton`: if `--game` can't find it, send `klip-cli audio-apps`.
3. **GNOME (Wayland)**, e.g. a live USB or a friend's PC: record and save a clip. Video
   timing there is uncompensated (0 ms) until measured; the hotkey portal on GNOME shows
   its own dialog.
4. **X11 session**: Bazzite's KDE is Wayland-only; on an X11 desktop the portal may not
   offer screen casting, and Klip has no X11 fallback yet (`ximagesrc`), so recording is
   expected to fail with a portal error there.
5. **HDR desktop**: both monitors here run KDE in HDR mode and clips looked normal in the
   tests (the screencast is SDR); an HDR *game* on Linux (e.g. with gamescope/Proton HDR)
   is unchecked.

## Manual checks for the human (M5)

1. **Play a killed recording**: `klip-cli record --session --out <folder>`, record a
   minute of anything, kill the recorder in Task Manager. Open the `Klip_Recording_…mp4`
   in Films & TV (or Media Player), VLC and your editor *before* starting Klip again: it
   should play (seeking may be slow in some players). Then run `klip-cli record` again
   (it finalises leftovers and prints `"event":"recovered"`), stop it, and open the file
   once more: it should now seek normally everywhere.
2. **Bookmarks**: during a recording run `klip-cli bookmark` a few times; VLC
   (Playback → Chapter) or mpv should list "Bookmark 1…n" at those moments.
3. **Long recording**: an hour of a real game with `--session`; `klip-cli status` →
   `session` shows size and fragments; stopping should take well under a second and the
   file should play to the end with sound in sync.

## Manual checks for the human (M6)

1. The app: `cd apps/klip-app && npm ci && npm run build`, then `cargo run -p klip-app`
   (it uses your real settings: default hotkeys Alt+F10 etc., buffers detected games).
   The first-run setup appears once. Tray icon grey while waiting, blue while a game is
   buffered, red dot while recording; left click opens the window, closing it keeps Klip
   in the tray, Quit in the tray menu ends it. Does the look match Photocraft / FilmCraft
   closely enough? The sun button switches follow system / Dark / Graphite / Light / OLED.
2. Hotkeys: in a real (non anti-cheat) game in fullscreen, Alt+F10 saves a clip; a key
   combination another app holds (e.g. the NVIDIA overlay's Alt+F10) shows as "already
   used by another app" in Settings → Hotkeys.
3. Overlay (Minimal, the default): in a borderless/windowed game, save a clip: the card
   appears top right of the game's monitor for ~3 s with a picture, the game keeps the
   keyboard (keep moving while it shows), and the G-Sync/FreeSync indicator (NVIDIA
   overlay "G-SYNC indicator" or the monitor's OSD) stays on, or tell me if it drops
   while the card shows. In an exclusive-fullscreen game: no card, just the sound.
   Full mode: the dot while recording; the quick-menu hotkey (set one) opens the menu,
   Esc closes it and the game gets the focus back. Settings → Overlay → "Show me".
4. Game detection: start a few games from Steam / Epic / Xbox app / Battle.net (no
   anti-cheat titles without asking): Home shows the right name and starts buffering;
   launchers and store clients don't count. Battle.net games have no common library
   folder: add them under Settings → Games.
5. Settings: "Start Klip with Windows" adds Klip to Task Manager's startup apps and
   unticking removes it; "Choose…" opens the folder picker; the microphone Test meter
   moves when you speak; a hotkey field takes a new combination (your old hotkeys don't
   fire while it listens).
6. Storage cap: in a test clip folder, set the size limit low (e.g. 1 GB) with a few GB
   of clips, star one old clip, save a new clip: the oldest unstarred clips appear in the
   Recycle Bin (restorable), the starred one stays.

## Manual checks for the human (M7)

1. Library with your real clips: `cargo run -p klip-app` (or the built app), Library tab.
   Clips are grouped by day; posters appear, and cards scrolled into view get their
   hover-scrub strips within a couple of seconds (a whole old library fills in in the
   background while no game runs). Move the mouse across a card: the picture follows,
   with the time in the corner. Does it feel right?
2. Search (name, game, tag), the filters on the left (Starred, dates, games, tags), sort
   and card size S/M/L.
3. Select a clip: rename it in Details (Enter), add tags, star it; check in Explorer that
   the file was renamed. Double-click opens it in your player; "Show in folder" opens
   Explorer with it selected.
4. Delete (Del): it lands in the Recycle Bin and can be restored (Klip shows it again
   after restoring, when the window gets focus). Shift+Del deletes permanently after a
   warning. Rename or delete a clip that is open in a player: Klip should show the error,
   nothing breaks.
5. Put a few MP4s from another program (OBS, ShadowPlay, phone) in the clip folder: they
   show up with length, resolution and pictures; a broken file shows "No preview".
6. Ctrl/Shift-click and Shift+arrows select several clips; Ctrl+A; the right-click menu.

## Manual checks for the human (M8)

1. **Open a real clip of yours in the editor** (Library → double-click): scrub, split,
   cut a middle piece, 2x a selection, add a Caption, drag the 9:16 window, then
   Export → YouTube. Play the file in your player: the cuts, the speed and the caption
   must match what the preview showed. Tell me how long the export took for how long a
   clip (shown in the dialog's summary and in the Home activity).
2. **An HEVC clip** (record with the HEVC codec in Settings → Recording, or any HEVC
   file dropped in the clip folder): opening it in the editor shows "Making a preview…"
   first (the H.264 proxy); after that the video plays. Report how long it took.
3. **Discord**: export a 30–60 s clip with the Discord preset and upload it to a
   Discord server without Nitro. It must be accepted (≤ 10 MB) and look watchable; if
   Discord's limit on your account differs, set Settings → Export → Discord size.
4. **A clip with Game + Microphone tracks**: set Mic to 0 % and export; the result
   should have no voice. Set Game to 0 %: only voice.
5. **Close the app while an export runs**: the file must not appear half-written
   (there is no `.part` left behind; the export is cancelled).

## Manual checks for the human (M9, the Advanced editor and montages)

Use the release app (`target\release\Start Klip.cmd`) and two of your own clips.

1. ✅ (2026-10-10: sound and sliders work) **Sound sliders (the M8 bug).** Open a clip that has your voice. In Normal mode
   press Play, then drag **Mic** to 0 %: your voice must go away while it plays; drag
   **Game** to 0 %: the game sound must go away. Tell me if you hear any echo or delay.
2. ✅ (2026-10-10) **Switch to Advanced.** In the Mode section click **Advanced**. The layout changes:
   Media/Effects/Main clip on the left, Inspector/History/Export on the right, the
   timeline at the bottom. Click **Normal** at the top of the preview: back; an
   orange **Advanced edits** badge appears once you made Advanced changes.
3. ✅ (2026-10-10, after a redo) **Montage.** Advanced → Media → tick two other clips → **Add 2 clips**. They appear
   one after the other on the "Montage" track, after your clip. Click the first of the
   two; in the Inspector under **Into the next clip** choose **Crossfade**. Play across
   the join: the pictures must blend for half a second, and both clips' sound must play.
4. ✅ (2026-10-11) **Zoom and move.** Select a montage clip, put the playhead in it, set **Zoom** to
   1.5 in the Inspector. Drag the blue frame on the preview to move the picture; scroll
   on it to zoom more. Click ◆ at another time, change Zoom there: it must glide
   between the two when you play.
5. ✅ (2026-10-11) **Sticker and music.** Media → **+ Sticker** (pick a PNG) and **+ Music** (pick an
   MP3). The sticker shows over the video for 3 s; the music plays on its own track.
   Talk over a part (a clip with your voice): the music must get quieter while you
   speak (Effects → Music under voices).
6. ✅ (2026-10-11) **Colour.** Effects → Colour: drag Saturation to 0: the preview turns grey (the
   text, if any, keeps its colours).
7. ✅ (2026-10-11) **Undo history.** Right side → History: every step has a name; click one further
   down to go back to it, then click a grey one above "Now" to go forward again.
8. ✅ (2026-10-11) **Export → Custom.** Export… → **Custom** → 1280 wide (720 follows), 30 fps,
   8000 kbit/s, H.264 → Export. Play the result: it must look like the preview, at that
   size (check the file's properties).
9. ✅ (2026-10-11: felt fine) **J / K / L.** With the timeline focused: L plays, L again plays 2x, K stops, J goes
   backwards. Tell me if anything feels sluggish (how long a montage, how many clips).
10. ✅ (2026-10-11: a 3:13 edit exported in ~20 s, ~10x realtime; all checks passed, found in the Library) **A montage of your own games.** Take the montage from steps 3–4 (two of your real
    clips, a crossfade, a zoom) and export it with **YouTube**. Open the file in your
    usual video player: the join must blend, the zoom must be there, both clips' sound
    must play, and it must appear in the Library next to your first clip. Tell me how
    long the export took and how long the montage is.

## Manual checks for the human (M9.7, Advanced mode like FilmCraft)

Use the release app (`target\release\Start Klip.cmd`) and a few of your own clips.
Quit and restart Klip first, so the new version runs.

1. ✅ (2026-10-11) **The layout.** Open a clip, switch to **Advanced**. It should look like FilmCraft's
   Editing workspace: Effect Controls top-left, the Program preview in the middle,
   Properties top-right; Project, the Timeline and two level meters along the bottom.
   Drag the dark gaps between panels to resize them; reopen the editor: the sizes stay.
2. ✅ (2026-10-11) **Clicks don't move clips.** Add two clips (drag them from **Project** onto the
   empty track above the video tracks, or tick them and click **Add**). Click a clip
   several times without moving the mouse: it must not move. Drag it: it moves.
3. ✅ (2026-10-11) **Rows never hide.** Add music (**+ Music** at the bottom of Project) and make the
   tracks taller by dragging the bottom edge of a track's name on the left. Scroll the
   timeline down: the music clip must still show and be clickable.
4. ✅ (2026-10-11; Dip to black unclear to the user: on an upper track it dips through what is below, not black; fix in M9.8, recheck) **Transitions on the timeline.** Hover the join between two touching clips: a blue
   **+** appears; click it: a striped "Crossfade" block sits on the join. Drag its left
   edge: it gets longer. Right-click it: switch to **Dip to black**, then **Remove**.
5. ✅ (2026-10-11) **Crossfade from your clip into the montage.** Click the montage's first clip. In
   **Properties**, under **From the clip before**, choose **Crossfade** (or click the
   **+** at the start of that clip). Play across it: your original clip must blend into
   the montage clip instead of cutting.
6. ✅ (2026-10-11; the user also found the WebView's own Print / Save as (HTML): M9.8) **Effect Controls.** Select a montage clip, put the playhead in it. In Effect
   Controls drag the blue **Zoom** number sideways: the picture zooms. Click the
   stopwatch next to **Motion** (it turns blue), move the playhead, drag Zoom again:
   diamonds appear on the right; play: it glides. Click the stopwatch again: the
   diamonds go and the zoom stays where it was.
7. ✅ (2026-10-11, after M9.7.6: pictures fill zoomed clips; a quiet PAYDAY 3 clip, peaks −41 dB in the file itself, showed almost no waveform: floor to −60 dB in M9.8) **Thumbnails, waveforms, meters.** Clips on video tracks show little pictures;
   sound and music show a light-blue waveform. Press Play: the two meters on the right
   of the timeline bounce with the sound.
8. ✅ (2026-10-11: the from-below crossfade, zoom, music and game sound all in the export; 10–15 s) **Export.** Export the edit with **YouTube** and play it: the from-below crossfade
   and the zoom must be there, like in the preview.

Tell me what looks or feels wrong, and anything that still doesn't feel like FilmCraft.

## Manual checks for the human (M9.8, FilmCraft feel)

Use the release app: quit Klip and start it again with `target\release\Start Klip.cmd`,
so the new version runs. Open a clip, switch to **Advanced**, add two or three clips from
**Project** so they sit next to each other on a track.

1. ✅ (2026-10-11) **The zoom stays.** Zoom the timeline in (Ctrl+wheel). Move or delete a clip: the
   zoom must not jump. Press **\\** (or the ↔ button): the whole edit fits again.
2. ✅ (2026-10-11) **Select several.** Click a clip, then Shift-click another: both get a white outline.
   Drag one: both move. Click empty space and drag a box over clips: they get selected.
   Press **Delete**: they go (Ctrl+Z brings them back).
3. ✅ (2026-10-11) **Ripple delete.** Select a clip in the middle and press **Shift+Delete**: it goes and
   the clips after it slide left to close the gap.
4. ✅ (2026-10-11) **Razor.** Press **C** (or the blade button top-left of the timeline): the pointer is a
   blade and a dashed line shows where it would cut. Click a clip: it is cut there. Hold
   Shift and click: every track is cut there, your original clip too. Press **V** to get
   the arrow back.
5. ✅ (2026-10-11) **Ctrl+K.** Put the playhead inside a clip and press **Ctrl+K**: everything under the
   playhead is cut (only the selected clips, if you selected some).
6. ✅ (2026-10-11) **Move a cut under a transition.** Put a **Dip to black** or a crossfade between two
   touching clips (right-click the striped block). Drag the middle of the striped block
   sideways: the cut moves with it, one clip gets longer and the other shorter.
7. ✅ (2026-10-11) **Dip to black is black.** With a Dip to black on a montage track above your clip,
   play across it: the picture must go fully black in the middle (not show your clip
   underneath), then come back. Export it: same.
8. ✅ (2026-10-11; Klip's own menus on Library cards and transitions checked still open) **No browser stuff.** Right-click anywhere outside a text box: no menu. Press Ctrl+P,
   Ctrl+S, Ctrl+F, F5: nothing happens (no Print, no Save as, no reload).
9. ✅ (2026-10-11) **Quiet waveforms.** Open your quiet PAYDAY 3 clip (11-39-03) in Advanced mode: its
   waveform should now be visible.

## Known bugs

- e2e `m3_hdr::sdr_game_on_hdr_desktop_is_unchanged` fails since the evening of
  2026-10-09 with every grey level ~10 % darker (patch 16 → 13, 64 → 59, checker
  60/90 → 54/82), identically with `main` before M6.12 (checked): the desktop's HDR /
  SDR-content-brightness state changed, the code didn't. To check with the human (SDR
  brightness slider) before touching the test.
