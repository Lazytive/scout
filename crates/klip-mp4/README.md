# klip-mp4

Writes MP4 files from packets that are already encoded (no re-encode): faststart files
for clips, fragmented files for full-session recordings, and turns a fragmented recording
into a regular MP4 in place. Decision records: `docs/adr/0003-own-mp4-writer.md`,
`docs/adr/0013-full-session-recording.md`.

## Clips (`write_mp4`, `save_mp4_atomic`)

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
- `Mp4Track::new(info, packets).named("Game").enabled(false)`: the name goes in `hdlr`
  (ffprobe `handler_name`), `enabled` in the `tkhd` flags (ffprobe `default` disposition).
  With more than one audio track, all audio tracks share alternate group 1, so players
  play the enabled one instead of all at once.
- `save_mp4_atomic`: writes `<name>.part`, fsyncs, renames; refuses to overwrite and
  removes the temp file on failure.

- `Mp4Options::markers`: bookmarks inside the clip become chapters (`moov/udta/chpl`,
  version 1, 100 ns units, at most 255), shown by ffprobe/VLC/mpv.

## Recordings
- `FragmentedWriter::new(out, tracks, origin, opts)` writes `ftyp` (+`iso5`) and the init
  `moov` (empty tables, `mvex`/`trex`); `write_fragment(parts)` writes one `moof` + `mdat`
  (`tfhd` default-base-is-moof, `tfdt` v1, `trun` with duration/size/flags/(cts) per
  sample) and flushes. Times are rounded from absolute timestamps against `origin`; a
  part's `next_dts` gives its last sample an exact duration. Input is validated before
  anything is written; after a write error the writer refuses more (`Poisoned`).
- `scan_fragmented(reader)`: init `moov` → `TrackInfo`s (round-trips what the writer
  wrote), complete fragments → `ScannedTrack::samples` + `runs`. Stops quietly at the
  first incomplete or inconsistent fragment; also reads fragments and the init that
  `finalize` already turned into `free`.
- `finalize(path, markers)`: truncate after the last complete fragment, append a regular
  `moov` (chunk per `trun`, empty edits, chapters), fsync, then `moof`s and the init
  `moov` become `free`, fsync. Repeating it after an interruption gives the same file; a
  finished file returns `already_final`.

## Reading clips (`probe`)
- `probe(reader)` → `Mp4Info`: movie duration, creation time, the first video track
  (codec name, width, height, average fps from samples / track duration), audio track
  count and handler names (`audio_names`: `Mix`, `Game`, `Microphone`, ...), chapter
  times and titles (`udta/chpl`: Klip's bookmarks are `Bookmark <n>`; the library
  treats other titles as events), whether it is fragmented. Reads
  box headers and the `moov` only (≤ 64 MiB), wherever it is (front, end, after
  fragments); works on files from other programs (unknown codecs are named by their four
  letters). A track or `udta` that doesn't parse is skipped; no `moov` is an error. Used
  by the library's info index (ADR 0015).

## Tests
Unit tests parse the produced boxes and check offsets point at the right payloads, edit
lists, composition offsets, `co64`, Opus `dOps`, chapters, hostile input; for recordings:
data offsets and `tfdt`s, init round trip, truncation at every byte of the last
fragments, every intermediate state of an interrupted finalise, 3000 mutated files;
`probe` (`info_tests.rs`): faststart and moov-at-end clips, live and finalised
recordings, truncation at every byte, 4000 mutated files, random bytes and random box
trees (proptest), a 64 MiB+ `moov` refused without reading it.
Real-media validation (ffprobe + decode) happens in the recorder's hardware tests.

Layer: pure.
