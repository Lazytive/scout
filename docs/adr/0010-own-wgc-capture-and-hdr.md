# 0010: own Windows.Graphics.Capture source and HDR tone-mapping

Status: accepted (M3, 2026-10-09).

## Context
Up to M2, video came from GStreamer's `d3d11screencapturesrc` in WGC mode. Measured on
the dev machine (both monitors in Windows HDR mode, SDR white 120 / 140 nits, peaks
400 / 648 nits):
- It stamps frames with its own clock slots, not WGC's capture time, so M2 had to poll it
  at 8x the capture rate and re-stamp on arrival (ADR 0008).
- It only delivers 8-bit BGRA. An HDR game (test game `--hdr`, FP16 scRGB swapchain) came
  out with every highlight above 80 nits flat white, and its SDR-range parts too bright
  when the game's paper white isn't 80 nits (WGC maps scRGB 1.0 to white).
- GStreamer's `d3d12screencapturesrc` DXGI mode samples scRGB, but its `linear` and
  `reinhard` tone maps both clip everything above SDR white (measured), and DXGI
  duplication can't capture a single window.
- SDR content on an HDR desktop comes out exact through all of them.

## Decision
- `klip_platform::GpuCapture` (`wgc.rs`): a free-threaded WGC frame pool on a D3D11 device
  that Klip creates on the encoder's adapter (the encoder's `adapter-luid`, else the
  monitor's). The device is wrapped as a `GstD3D11Device` (`gst_d3d11.rs`, the GStreamer
  D3D11 library's C API, dynamically linked) and shared with the pipeline through a
  `GstContext`, so frames never leave the GPU.
- Each WGC frame is drawn once by a full-screen-triangle shader into a texture from a
  GStreamer D3D11 buffer pool at the output size: client-area crop (DWM extended frame
  bounds vs client rect), scale, letterbox on aspect changes. The buffer's time is when
  the frame reached us (QPC, the clock of our audio stamps), bounded by WGC's
  `SystemRelativeTime`, on the recording's running time; it goes into an `appsrc` (leaky, 4 buffers), then `videorate drop-only` and
  the frame grid as before. When every output buffer is still in use downstream, the
  frame is dropped and counted (`dropped_busy`), never blocking WGC.
- **HDR** (target's monitor in HDR mode): the pool captures FP16 scRGB. The shader scales
  so SDR white (Windows' SDR content brightness) is 1.0, clips negative (out-of-gamut)
  components, measures the frame's peak (every 8th pixel, atomic max into a UAV, read
  back two frames later without stalling), and when the scene has content above SDR
  white, rolls the brightest component off above a knee in sRGB-encoded space
  (`k + e / (1 + e (L-1) / ((1-k)(L-k)))`, knee 0.8, `L` = encoded scene peak) so the
  scene peak lands on white, keeping colour ratios; then sRGB-encodes. The peak estimate
  rises within a few frames and falls over seconds, and is capped at the display's peak
  (brighter content isn't visible on it either). With no HDR content the knee is 1.0 and
  the conversion is exact.
- `CaptureApi::Auto` (default) uses this source and falls back to `d3d11screencapturesrc`
  if it can't start; `--capture-api klip|gst-wgc` forces one.

## Consequences
- Measured (`tests/m3_hdr.rs`, test game on the 140-nit/648-nit monitor): SDR game on
  the HDR desktop: all six gray patches exact, checker 59/89 for 60/90 (encode
  rounding), scene peak = SDR white. HDR game (paper white = SDR white): SDR parts
  unchanged, highlights 80/160/320 nits → 199/236/249, 640 nits (display peak) and above
  → 255; scene peak measured 648 nits.
- **Timestamps** (A/B with an idle GPU, test game on the 120 Hz monitor, `m2_audio`
  sync test): WGC's `SystemRelativeTime` is the display time, on average 7.1 ms (max 8; one refresh is
  8.3 ms) *after* the frame reaches us; stamping with it put video +9.9 to +13.3 ms after the
  game finished the frame (A/V −9 to −13 ms). Arrival time: +3.9 ms (A/V −3.4 ms);
  min(WGC, arrival), the chosen stamp: +4.6 ms (A/V −3.8 ms). The old polled source:
  +4.2 to +5.9 ms (A/V −3.1 to −5.6 ms). Under heavy GPU load from another game every
  source drifts to +13 to +26 ms (composition itself is late).
- The tone curve is a choice, not a standard; it may need tuning by eye on real HDR
  games (human checklist).
- A window that moves to a monitor with a different HDR state keeps the settings of the
  monitor it started on.
- Exclusive fullscreen and lost WGC sessions are handled by the fallbacks of M3.6.
