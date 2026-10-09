# 0007: test strategy

Status: accepted (M0)

## Decision
- **Unit/property tests** for pure crates run in `cargo xtask ci` (no GPU or display needed).
- **Test game** `klip-testgame` (wgpu + winit): a 34-cell barcode on every frame
  (2 reference cells + 24-bit frame counter + CRC-8), a moving bar and a scrolling checker
  pattern for motion, and a white flash every second (a beep joins it in M2). Options for
  size, fps, windowed/borderless/exclusive fullscreen; HDR later.
- **Clip checks** (`klip-testkit`): decode a clip with ffmpeg to 8-bit gray, read the
  barcodes, and check counter continuity, duration, fps and track count (later A/V sync
  from flash vs beep). ffmpeg/ffprobe are only external oracles, called as processes.
- **Hardware acceptance** (`cargo xtask e2e`): `#[ignore]`d tests run with
  `--include-ignored`, single-threaded, because they need a GPU encoder and an interactive
  desktop. Each prints its measurements (machine, settings, numbers) so results can go in
  `ROADMAP.md`.

## Consequences
`xtask ci` stays green on machines without GPUs; the e2e gate is required before a capture
or encode task is marked done.
