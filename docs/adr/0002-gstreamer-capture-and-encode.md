# 0002: GStreamer for capture and encode; encoder allowlist

Status: accepted (M0); element names verified against GStreamer 1.28.6 MSVC on 2026-10-09.

## Context
Klip needs OS-API capture (rule 1), hardware encoders from three vendors on two OSes, audio
capture and later an export pipeline, while staying MIT/Apache (rule 5). Options: libobs
(GPL, ruled out), FFmpeg libraries (licence depends on the build; GPL builds are common),
raw per-vendor SDKs (a lot of code per vendor and OS), GStreamer (LGPL, dynamically linked,
official Rust bindings `gstreamer-rs` under MIT/Apache).

## Decision
GStreamer ≥ 1.24 via `gstreamer-rs`. Windows elements, verified with `gst-inspect-1.0`:
- Video capture: `d3d11screencapturesrc` (`capture-api=wgc`, `window-handle` +
  `window-capture-mode=client`, `monitor-index`, `show-border=false`). Output is
  `video/x-raw(memory:D3D11Memory), format=BGRA`. Convert/scale on the GPU with
  `d3d11convert` to NV12 at a fixed output size, so window resizes never change encoder
  caps.
- NVIDIA: `nvd3d11h264enc` / `nvd3d11h265enc` / `nvd3d11av1enc` (D3D11 input, zero-copy);
  CUDA-mode `nvh264enc` etc. as fallback. AMD AMF and Intel QSV element names are verified
  in M3 on hardware that has them. Media Foundation `mfh264enc` is the vendor-neutral
  fallback.
- Parse with `h264parse`/`h265parse`/`av1parse` to `stream-format=avc|hvc1, alignment=au`
  so `codec_data` in caps is exactly the `avcC`/`hvcC` payload the MP4 writer needs.
- **Encoder allowlist** in `klip-probe`: only vendor, OS and (Linux) openh264 encoders can
  ever be selected. `x264enc`, `x265enc`, `avenc_*` H.264/HEVC encoders and other software
  encoders are denied by name, even though the official GStreamer Windows installer ships
  some of them. `cargo xtask audit` fails if a denied name appears in product code
  outside the denylist itself, or in a bundled plugin directory.
- Keyframe interval 1 s (`gop-size = fps`), no B-frames (`bframes=0`) for the replay
  buffer: save cut points are at most 1 s early and PTS == DTS. The MP4 writer still
  supports composition offsets for other sources.

## Consequences
Users on Windows need the GStreamer runtime; the installer (M13) bundles the LGPL runtime
DLLs and an audited plugin subset, dynamically linked. WGC delivers a frame only when the
captured content changes (measured: about 1 fps on a static desktop), so streams are
variable frame rate; see the M1 notes in `crates/klip-capture/README.md`.
