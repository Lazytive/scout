# klip-testkit

Test support only (used by tests, the test game and benchmarks; never shipped).

- `barcode`: 34 cells in a band at the top of each frame: white reference, black
  reference, 24-bit frame counter + CRC-8 (MSB first). Positions are fractions of the
  frame, cells are inset, and the reader uses an adaptive threshold, so codes survive
  scaling, chroma subsampling and H.264 at normal bitrates. `render_bgra` draws the same
  layout on the CPU for synthetic encoder tests.
- `oracle`: ffprobe/ffmpeg as **external processes** (never linked or shipped):
  stream info, per-frame times, decoding to gray with `-fps_mode passthrough` (so ffmpeg
  doesn't add or drop frames) and reading barcodes. `top_level_boxes` checks faststart
  order without an oracle.
- `continuity`: steps between consecutive counters → duplicates, drops (big steps),
  reordering, unreadable frames.
- `audio`: the test beep (1 kHz, 60 ms, starts at the peak), `render_beeps_s16` for
  synthetic sources, `onsets` (threshold crossing after a quiet gap; sample-exact on clean
  input, AAC pre-echo stays below the threshold).
- `sync`: A/V sync of a clip against the test game's frame and beep logs. The median of
  `clip time − game time` over all barcoded frames maps the game clock onto the clip
  (absorbing capture latency); each beep's error is its detected onset minus that
  prediction.
- `oracle` also decodes audio (`decode_audio_mono`) and lists frame times of any stream
  (`frame_times(path, "a:0")`).

Dev tooling, so `arithmetic_side_effects` is allowed in this crate (ADR 0001).
