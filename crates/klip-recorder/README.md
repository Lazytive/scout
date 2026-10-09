# klip-recorder

Ties capture, the replay ring and the MP4 writer together.

- `RingSink`: a `klip-buffer` ring behind a mutex that implements
  `klip_capture::PacketSink`. Leading delta frames before the first keyframe are dropped
  silently; other rejects are counted and logged.
- `save_from(sink, dir, length)`: snapshot under the lock (refcount copies only), then
  write `Klip_YYYY-MM-DD_HH-MM-SS[_n].mp4` atomically outside the lock. Measured 3–9 ms for
  10–30 s clips at 20 Mbit/s 720p60 on the dev machine (NVMe), see ROADMAP.
- `Recorder`: starts a `CaptureSession`, owns the sink, serialises saves, reports status
  JSON, and implements `klip_control::Handler` for `clip.save` (`last_ms`, 1 s–1 h),
  `recorder.status`, `recorder.stop`, `ping`.
- `estimate_buffer_bytes`: RAM estimate / default byte cap = bitrate x 1.5 (VBR peak) x
  (length + 1 s).

Tests: unit tests for saves (unique names, atomic files, empty buffer); hardware test
`tests/synthetic_encode.rs` pushes 600 barcoded frames through the real encoder, ring and
writer and checks every frame comes back exactly once, and that cuts start on keyframes.

Layer: media engine.
