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

Dev tooling, so `arithmetic_side_effects` is allowed in this crate (ADR 0001).
