# Roadmap and honest status

Last updated: 2026-10-09 (end of M1).

| | Milestone | State |
|---|---|---|
| M0 | Repo, rules, gates | **done** |
| M1 | Recorder CLI (Windows, NVENC) | **done**: acceptance passes |
| M2 | Audio (game/per-app/mic tracks, A/V sync ≤ 1 frame) | next |
| M3 | All GPUs + fallbacks, HDR tone-mapping | |
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
window at 60 fps, test game at 120 fps. Numbers from `cargo xtask e2e` on 2026-10-09.

- `klip-cli record --buffer 120s` + `klip-cli clip --last 30s`:
  - save time (snapshot + write + fsync + rename): **9.0 ms** for a 30.98 s, 9.1 MB
    clip; 3.9 ms for a 10 s clip. CLI round trip incl. process start: 53 ms.
  - Clip is faststart (`ftyp, moov, mdat`), H.264 1280x720, 1856 frames, 59.9 fps
    average, 30.98 s (cut on the keyframe at or before the start, so 30–31 s).
  - Frame continuity: all 1856 barcodes readable, none repeated, none out of order, no
    gap longer than one capture tick (game-counter steps 1–4 around the expected 2).
- Synthetic exactness test (600 known frames → NVENC → ring → MP4 → ffmpeg decode):
  600/600 frames back, in order; a 2 s cut is exactly 120 frames starting on a keyframe.
  Save 3.0 ms.
- Encoder probe on this machine: all of `nvd3d11{h264,h265,av1}enc`,
  `nv{h264,h265,av1}enc` (CUDA) and Media Foundation H.264/HEVC MFTs pass a 1 s encode
  (0.3–1.4 s per probe).
- Replay ring: GOP-aligned eviction, byte cap, hostile input refused (unit + property
  tests). MP4 writer: box layout, offsets, edit lists, ctts, co64, Opus dOps (unit
  tests). Control channel: token auth, caps, malformed input (unit tests).

## Approximate / not done yet

- **No audio** (M2).
- **Timestamps follow WGC frame arrival**: intervals jitter (14.6–18.6 ms p1–p99 in one
  run, exactly 16.67 ms in another). If the game renders below the capture rate, the
  source sometimes repeats a frame and leaves gaps (VFR). Planned: snap to the frame grid
  / CFR option, decided with A/V sync in M2.
- Static content gives ~1 fps (WGC only sends changed frames), so desktop capture is VFR.
- Only NVENC was tested on real hardware. AMF/QSV/VA-API property maps are unverified
  (M3). Hybrid laptops, exclusive-fullscreen fallback and HDR are M3.
- DXGI lists the RTX 4080 SUPER twice on this machine (virtual display drivers from
  Parsec / Virtual Desktop are installed); harmless now, dedupe by LUID in M3.
- Not measured yet: FPS cost on a game, CPU and RAM of the recorder (`cargo xtask bench`
  arrives with M3/M6). RAM for the ring is the estimate `bitrate x 1.5 x (buffer + 1 s)`
  = 454 MB cap for 120 s at 20 Mbit/s; actual use at 33 s buffered was 9.6 MB of packets.
- If the captured window closes, the recorder keeps the buffer (clips still save) but
  doesn't recover capture; restart/re-target logic comes with game detection (M6).
- Linux: nothing yet (M4).

## Known bugs

None open.
