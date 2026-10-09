# Roadmap and honest status

Last updated: 2026-10-09 (end of M2).

| | Milestone | State |
|---|---|---|
| M0 | Repo, rules, gates | **done** |
| M1 | Recorder CLI (Windows, NVENC) | **done**: acceptance passes |
| M2 | Audio (game/per-app/mic tracks, A/V sync ≤ 1 frame) | **done**: acceptance passes |
| M3 | All GPUs + fallbacks, HDR tone-mapping | next |
| M4 | Linux (portal + PipeWire, VA-API) | |
| M5 | Full-session recording + bookmarks | |
| M6 | Desktop app shell (Tauri) | |
| M7 | Library | |
| M8 | Editor, Normal mode + export presets | |
| M9 | Editor, Advanced mode | |
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
- Encoder probe on this machine: all of `nvd3d11{h264,h265,av1}enc`,
  `nv{h264,h265,av1}enc` (CUDA) and Media Foundation H.264/HEVC MFTs pass a 1 s encode.

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

## Approximate / not done yet

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
- Only NVENC was tested on real hardware. AMF/QSV/VA-API property maps are unverified
  (M3). Hybrid laptops, exclusive-fullscreen fallback and HDR are M3.
- DXGI lists the RTX 4080 SUPER twice on this machine (virtual display drivers from
  Parsec / Virtual Desktop are installed); harmless now, dedupe by LUID in M3.
- Not measured yet: FPS cost on a real game, recorder RAM (estimate for the ring:
  `(video + audio bitrate) x 1.5 x (buffer + 1 s)` = 457 MB cap for 120 s at 20 Mbit/s +
  one AAC track).
- If the captured window closes, the recorder keeps the buffer (clips still save) but
  doesn't recover capture; restart/re-target logic comes with game detection (M6).
- Linux: nothing yet (M4).

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

## Known bugs

None open.
