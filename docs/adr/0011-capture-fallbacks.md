# 0011: capture fallbacks (window → monitor → DXGI)

Status: accepted (M3, 2026-10-09).

## Context
Window capture (WGC) can't always see a game: in true exclusive fullscreen nothing new
arrives, some windows can't be captured at all, and a capture session can die. A
replay buffer that silently stops recording is the worst failure for a clipper. Klip
must stay on OS capture APIs (AGENTS.md rule 1), so the fallbacks are other OS APIs:
WGC monitor capture and DXGI Desktop Duplication.

## Decision
- A supervisor thread in the recorder (`klip_recorder::supervisor`) checks the video
  capture twice a second: frames produced, errors, the window (exists, minimised,
  covers its monitor, monitor), focus, and the shell's "D3D full screen" state. A pure
  function (`decide`, unit-tested) picks the action:
  - window → its monitor: exclusive fullscreen while it has focus; no frame for 3 s
    while covering its monitor; capture failed while the window exists;
  - monitor → window: the window no longer covers the monitor (and isn't exclusive);
  - monitor → another monitor: the window moved;
  - WGC monitor → DXGI: exclusive fullscreen and still no frame for 3 s (or failure).
  A user-chosen monitor is never switched. At least 5 s pass between switches.
- Recorder start: a window whose capture can't start is recorded through its monitor.
- A switch stops the video pipeline and starts a new one with the same encoder and the
  output size pinned at start (`VideoConfig::output`), on the same time base. The
  encoder's configuration record comes out identical, so the ring keeps its track and
  everything buffered; a clip spans the switch with a gap of ~0.25 s. A switch that
  fails goes back to the previous mode. Switches are listed in `status.video`.
- DXGI uses GStreamer's `d3d12screencapturesrc` (DXGI mode, frames to the D3D11 chain via
  `d3d12download`): on an HDR desktop it samples scRGB at the SDR white level, so SDR
  content stays exact; the D3D11 source's DXGI mode washed it out (measured 78/115 for
  60/90). HDR content is clipped at SDR white on this path (no tone-map yet).

## Consequences
- Measured (`tests/m3_fallback.rs`, test game borderless on the secondary monitor):
  the game stops drawing for 5 s → switch after 9.3 s of recording (3 s stall + tick),
  restart ~0.25 s, one clip holds frames 111–684 across the freeze, all readable and in
  order. DXGI monitor capture of the game: barcodes readable, gray patches exact.
- A paused game showing a static picture in borderless fullscreen also switches to
  monitor capture after 3 s; that records the same picture plus anything on top of it
  (notifications), which is acceptable.
- Exclusive fullscreen itself isn't tested automatically (it would take over a monitor
  and the focus); its rule is unit-tested and on the human checklist.
- Restarts open new encoder sessions; NVENC/Media Foundation interplay is covered by
  ADR 0009 (MF is started before NVENC).
