# 0003: own pure-Rust MP4 writer for clip saves

Status: accepted (M0)

## Context
Saving a clip means taking encoded packets that are already in RAM and writing a faststart
MP4 (moov before mdat) without re-encoding, in under 1 s. Options: (a) for each save, build
a GStreamer pipeline `appsrc ! mp4mux faststart=true ! filesink`; (b) write the MP4
ourselves.

## Decision
(b) `klip-mp4`, a small ISO-BMFF writer. Every sample size and timestamp is known before
writing, so the `moov` box can be computed first and the file written in one sequential
pass: faststart for free, with no temp-file copy (qtmux faststart writes mdat to a temp
file and copies it). Codec configuration (`avcC`, `hvcC`, `av1C`, AAC
`AudioSpecificConfig`) comes verbatim from the parser caps, so the writer only wraps bytes
in boxes. Supported now: H.264 (`avc1`), HEVC (`hvc1`), AV1 (`av01`), AAC (`mp4a` +
`esds`); Opus (`Opus` + `dOps`) if ADR 0005 picks it. Edit lists keep tracks aligned when
one starts later; `co64` is used automatically above 4 GiB. Full-session recording (M5)
uses fragmented MP4, decided then (`isofmp4mux` from gst-plugins-rs, MPL-2.0, is
available).

## Consequences
The clip path is GStreamer-free and unit-testable/fuzzable. Output is checked with ffprobe
and by decoding (test oracle). We own MP4 correctness, so every box layout follows
ISO/IEC 14496-12/-15 and is covered by a test.
