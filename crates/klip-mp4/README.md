# klip-mp4

Writes faststart MP4 files from packets that are already encoded (no re-encode). Used for
every clip save. Decision record: `docs/adr/0003-own-mp4-writer.md`.

- One sequential pass: `ftyp`, `moov`, `mdat`. The `moov` is computed first because every
  sample size and timestamp is known.
- Sample entries: `avc1`+`avcC`, `hvc1`+`hvcC`, `av01`+`av1C` (codec data verbatim from the
  parser caps), `mp4a`+`esds` (AAC), `Opus`+`dOps` (converted from `OpusHead`). Video gets
  `colr`/`nclx` when colorimetry is known, and `pasp`.
- Timing: video timescale 90 kHz, audio timescale = sample rate, movie timescale 1 kHz.
  Sample times are rounded from absolute timestamps, so there is no drift. `ctts`
  (version 1, signed) only when PTS ≠ DTS; `stss` only when some samples aren't sync
  samples.
- Tracks are aligned on the earliest first PTS; a track that starts later gets an empty
  edit (`elst` media_time −1), and B-frame streams get `media_time` = first composition
  offset.
- Samples are interleaved by decode time, one sample per chunk. `co64` and a 64-bit
  `mdat` header are chosen automatically above 4 GiB.
- `save_mp4_atomic`: writes `<name>.part`, fsyncs, renames; refuses to overwrite and
  removes the temp file on failure.

Tests parse the produced boxes and check offsets point at the right payloads, edit lists,
composition offsets, `co64`, Opus `dOps`, hostile input. Real-media validation (ffprobe +
decode) happens in the recorder's e2e tests.

Layer: pure.
