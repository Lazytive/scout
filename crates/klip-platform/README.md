# klip-platform

OS integration and the **only** crate allowed `unsafe` (AGENTS.md rule 3; enforced by
`cargo xtask layers`). Each `unsafe` block has a `// SAFETY:` comment.

Windows (Win32/DXGI via the `windows` crate):
- `list_gpus`: DXGI adapters (name, PCI vendor/device, LUID, VRAM, software flag, driver
  version), deduplicated by LUID. Virtual display drivers (Parsec, Virtual Desktop) make
  DXGI list the real GPU again under a *different* LUID; that copy has no outputs.
- `find_windows` / `window_info`: visible top-level windows, pid, client size and screen
  position, minimised, the monitor it is on. `WindowInfo::covers(monitor)` tells
  borderless/exclusive fullscreen. Titles are personal data: callers must not log them.
- `list_monitors`: HMONITOR, position/size, primary flag, the adapter LUID that drives it
  (DXGI outputs), Windows HDR on/off (`IDXGIOutput6` colour space), SDR white level
  (DisplayConfig, "SDR content brightness") and peak luminance.
- `exclusive_fullscreen_active`: the shell's "D3D full screen" notification state.
- `enable_dpi_awareness`: per-monitor-v2, so sizes are physical pixels.
- `list_processes`: pid, parent pid, executable name (Toolhelp snapshot).
- `audio_sessions`: processes with an audio session on any active output device (WASAPI
  session manager; system sounds and expired sessions left out), and whether each is
  playing. Used to pick apps for audio capture; exe names are shown to the user only.
- `AudioCapture` (`wasapi.rs`): a WASAPI capture thread for an output's loopback, a
  process tree (process loopback, include or exclude; async activation with a COM
  completion handler) or an input device. Float at the requested rate (AUTOCONVERTPCM),
  event-driven, every packet stamped with WASAPI's QPC capture time. Silence is generated
  while a stream delivers nothing; a failed or lost device becomes silence + a notice and
  is retried every 2 s. Packets that overlap generated silence are trimmed.
- `GpuCapture` (`wgc.rs`, ADR 0010): Klip's own Windows.Graphics.Capture source for a
  window (client area) or monitor. D3D11 device on a chosen adapter, shared with
  GStreamer (`gst_d3d11.rs`: `GstD3D11Device` wrap, `GstContext`, D3D11 buffer pool,
  device lock); per frame one shader pass (crop, scale, letterbox; HDR monitors: FP16
  capture, scene-peak measurement, tone-map to SDR) into a pooled texture handed to a
  `GpuFrameSink` with WGC's QPC capture time. Counters in `GpuCaptureStats`.
- `qpc_now_ns` (the clock of those stamps) and `process_loopback_supported` (build 19041+,
  via `RtlGetVersion`).

Gotcha: the `windows` crate implements `Drop` for `PROPVARIANT` (it calls
`PropVariantClear`). The process-loopback activation PROPVARIANT points at a stack struct,
so it is wrapped in `ManuallyDrop`; otherwise the heap is corrupted (found by
`audio_capture_is_continuous_and_qpc_stamped`). Diagnostic: `tests/loopback_offset.rs`
(`diag_`, needs `KLIP_DIAG_PID`) compares process and system loopback timing.

Linux (`linux_impl.rs`, `screencast.rs`; ADR 0012):
- `list_gpus`: DRM cards from sysfs (PCI vendor/device ids, driver, PCI slot packed into
  `luid`); names from NVIDIA's `/proc/driver/nvidia/gpus/*/information`, else hwdata's
  `pci.ids`; driver version for NVIDIA (`/sys/module/nvidia/version`), VRAM for amdgpu.
- `list_processes`: `/proc/<pid>/stat` (parent pid parsed after the last `)`) and the
  `exe` link's file name (else `comm`).
- `qpc_now_ns`: CLOCK_MONOTONIC (`rustix`), the clock of GStreamer's system clock and
  PipeWire's stream times.
- `ScreenCast`: an xdg-desktop-portal ScreenCast session (`ashpd`, `async-io` backend,
  blocked on with `pollster`): source types, cursor mode, persist mode "until revoked",
  restore token in and out; streams (PipeWire node, monitor/window, logical position and
  size) and the PipeWire remote fd. Dropping it closes the session. A cancelled dialog
  is `PlatformError::Cancelled`; portal replies are handled inside `catch_unwind`.
- `ScreenCast::is_closed`: a thread owning the portal session waits for the desktop's
  `Closed` signal (the user stopped sharing) or for the `ScreenCast` to be dropped (then
  it closes the session).
- `AudioCapture` (`pipewire_audio.rs`, ADR 0008): PipeWire capture on its own thread and
  main loop: output monitor, input device, or apps' playback streams linked by Klip
  (include a process tree / everything except it; process ids from the client's
  `pipewire.sec.pid`, set by the daemon). Stamped with the graph cycle time minus the
  stream delay (CLOCK_MONOTONIC); silence while nothing plays; reconnects every 2 s.
- `audio_sessions`: processes with a playback stream, active when one is running (two
  registry round trips; nodes bound for their state).
- `timeline.rs`: the silence/overlap timeline shared with WASAPI.
- Monitors and windows return `Unsupported` (Wayland has no window list).

Only documented OS APIs are used (WGC is the OS capture API). Nothing here touches
another process's memory or rendering (rule 1).
