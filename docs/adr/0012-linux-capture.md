# 0012: Linux capture (screen-cast portal + PipeWire) and encode

Status: accepted (M4, 2026-10-09).

## Context
On Wayland an app may not read the screen or enumerate other apps' windows; the only
sanctioned way is the xdg-desktop-portal ScreenCast interface: the desktop shows its own
dialog, the user picks a monitor or window, and frames arrive as a PipeWire stream. That
is also the only capture API AGENTS.md rule 1 allows on Linux. Encoders: NVENC through
GStreamer's CUDA-mode elements, VA-API (AMD/Intel), Cisco's openh264 as the software
fallback (rule 6).

Dev box: Bazzite 44 (Fedora, KDE Plasma 6.7.5 Wayland), NVIDIA RTX 4080 SUPER driver
615.71, two 3840x2160 monitors at scale 1.7, GStreamer 1.28.7, PipeWire 1.6.9, portal
ScreenCast v5.

## Decision
- **Portal session** in `klip_platform::ScreenCast` (`ashpd` 0.13, `async-io` backend,
  blocked on with `pollster`; no tokio): `SelectSources` with persist mode "until revoked"
  and the stored restore token, `Start`, `OpenPipeWireRemote`. The CLI keeps the token in
  `$XDG_STATE_HOME/Klip/screencast-token` (atomic write; size-capped, printable ASCII
  only when read back), so the dialog appears once. One session per recording, shared
  (`klip_capture::PortalSession`, an `Arc`) by every pipeline: encoder fallbacks reconnect
  to the same node without asking again. Portal replies are parsed inside
  `catch_unwind` (the crate asserts on malformed replies).
- **Pipeline**: `pipewiresrc fd=… path=<node>` → `video/x-raw` (system memory) →
  `videorate drop-only` → `framerate=F/1` → encode chain. The output size comes from the
  stream's first caps: the chain is built and linked at a provisional size and a probe on
  the source pad points its size capsfilters (`EncodeChain::set_size`) at
  `fit_size(stream)` while that caps event passes, before any frame reaches the chain.
  (Blocking the first buffer until the chain is built instead made `pipewiresrc` fail.)
  Later size changes are scaled to the first size.
- `on-disconnect` stays `none`: its `error`/`eos` modes fire whenever PipeWire removes
  all buffers, which also happens on every renegotiation (KWin renegotiates right after
  the first frame).
- **Frame times**: `pipewiresrc` stamps buffers with the compositor's frame time
  (CLOCK_MONOTONIC, PipeWire's clock) minus the pipeline base time; the recording uses
  GStreamer's system clock, which is CLOCK_MONOTONIC too, so `qpc_now_ns` on Linux is
  CLOCK_MONOTONIC. The capture time is kept in the arrival meta and snapped to the frame
  grid as on Windows (`KLIP_CAPTURE_STAMPS=arrival` stamps on arrival for A/B runs).
- **Encode chain on Linux** (`convert_front`): NVENC: `glupload ! glcolorconvert` →
  RGBA → `glcolorscale` → `glcolorconvert` → NV12 GL memory → `nvh264enc` (NVENC takes GL
  textures). GStreamer's CUDA converters need NVIDIA's runtime compiler `libnvrtc`, which
  desktops don't have (Fedora's nvcodec registers no `cudaconvertscale` without it).
  `glcolorscale` only scales RGBA, hence the two conversions. VA-API: `vapostproc` → NV12
  VA memory. openh264: `videoconvertscale` → I420 (CPU).
- Allowlist additions: VA low-power entry points (`vah264lpenc`, `vah265lpenc`,
  `vaav1lpenc`; Intel Arc encodes AV1 only this way) and VA's per-device names
  (`varenderD129h264enc` = `vah264enc` on `/dev/dri/renderD129`).

## Measured (dev box above, 2026-10-09)
- `every_working_encoder_round_trips_exactly`: `nvh264enc`, `nvh265enc`, `nvav1enc`,
  `openh264enc` each return all 240 barcoded frames exactly once.
- M1 acceptance through the portal (test game borderless fullscreen 3840x2160 @ 120 fps
  on the second monitor, capture 60 fps, scaled to 1280x720, `nvh264enc`): 15.27 s clip,
  916 frames, 60.00 fps, every barcode readable, no repeats, no reordering, frame
  interval p99 17.2 ms.
- Recorder CPU, 3840x2160 stream → 1920x1080 @ 60 fps NVENC: **31 % of one core** with
  system memory (KWin downloads each frame, we upload it again); RSS 457 MB. With
  DMA-BUF (`KLIP_PORTAL_CAPS=video/x-raw(memory:DMABuf)`: KWin offers NVIDIA-tiled AR24,
  `glupload` imports it directly) 17 % and 330 MB, **but** the M1 test then sees 26
  repeated frames and irregular frame times: the buffers carry no fence and NVIDIA has no
  implicit sync, so a frame can be read before the compositor finished it. System memory
  stays the default until explicit sync (PipeWire `SyncTimeline` meta) is handled.

## Frame timing (measured)
KWin's monitor screencast re-renders the scene into the stream buffer when a repaint is
scheduled and stamps it with the output's *previous* presentation time
(`renderLoop()->lastPresentationTimestamp()`, read from KWin 6.7's source), so the PTS
says nothing about when the content was drawn. With the test game (`m2_audio.rs` stamp
latency; GPU-finished frame log as reference), frames reach Klip **+27.6 to +27.9 ms**
after the game finished them (system memory; +32.7 ms with DMA-BUF), steady within a run
(beep errors spread < 1 ms); a `queue` right after `pipewiresrc` changes nothing, so the
delay is in the compositor. Decision: portal frames are stamped **on arrival minus a
compositor latency**: 24 ms on KDE (so video lands at about +4 ms after the GPU, like
Windows), 0 on unmeasured desktops, `KLIP_VIDEO_LATENCY_MS` overrides. Result on an
otherwise idle desktop: A/V −5.1 ms (system audio), and with per-app game audio +0.07,
−8.84 and −11.91 ms in three runs (each within ±0.9 ms); video lands +1 to +12 ms after
the GPU depending on the run (KWin's scheduling).
With another app busy on the other monitor (Firefox playing video) the screencast
latency rose by ~13 ms, so the compensation is approximate under load (ROADMAP).

## Consequences
- The first recording on a machine shows the desktop's dialog; tests keep their own token
  (`~/.local/state/klip-e2e/portal-<monitor>.token`) and fail early with instructions if
  the stream isn't the test game's monitor (KDE also offers "Full Workspace").
- Monitors and windows can't be listed or targeted by handle on Wayland; `--window-*` and
  `--monitor` are Windows options. The portal's stream info (position, logical size) is
  reported in the `record` ready line.
- Stopping the share from the desktop (KDE's tray) ends the portal session; its
  `Closed` signal becomes a capture error (`watch_portal`). Not testable automatically
  (only the desktop can close it): human checklist.
- VA-API paths are untested here (no AMD/Intel GPU); HDR desktops: see ROADMAP.
