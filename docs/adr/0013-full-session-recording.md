# 0013: Full-session recording (fragmented MP4) and bookmarks

Status: accepted (M5, 2026-10-09).

## Context
Besides the replay buffer, Klip records whole sessions (start/stop) with bookmarks that
mark moments. A recording can run for hours, so it must be written to disk as it goes,
and a crash, a killed process or a power cut must not lose it: the file on disk has to
play as it is. When recording stops normally the file should be an ordinary MP4 that every
player and editor reads. Bookmarks must reach the editor (M8) and, ideally, other tools.

## Decision
- **Fragmented MP4 while recording** (`klip_mp4::FragmentedWriter`, ISO/IEC 14496-12
  movie fragments): `ftyp` (adds `iso5`) + init `moov` (empty sample tables, `mvex` with
  one `trex` per track), then per fragment a `moof` (`mfhd`, one `traf` per track:
  `tfhd` with default-base-is-moof, `tfdt` v1, `trun` with per-sample duration, size,
  flags and, if needed, signed composition offsets) and its `mdat`. Track layout, names,
  enabled flags and the audio alternate group are the same as in clips (ADR 0008).
- **Timing**: one origin for the file (its first video keyframe on the capture timeline,
  time 0 of every track). Decode times are rounded from absolute timestamps in each
  track's timescale (90 kHz video, sample rate for audio), and a sample's duration is the
  distance to its successor, so there is no drift and `tfdt` always equals the previous
  fragment's end. The writer holds each track's newest packet back until its successor
  arrives. Audio before the origin is dropped; a later audio start is simply its `tfdt`.
- **Fragments** end at video keyframes (one per second with our GOP), or after 4 s or
  64 MiB when keyframes are rare or video stalls (a lone held-back packet of a quiet
  track doesn't count). Each fragment is flushed to the OS when written, so a killed
  process loses at most the fragment being written; the file is fsynced every 10 s
  (bounds what a power cut loses).
- **Finalising in place** (`klip_mp4::finalize`): scan the file, cut off anything after
  the last complete fragment, append a regular `moov` (sample tables from the `trun`s, one
  chunk per `trun`, empty edits for tracks that start later, chapters), then change each
  `moof` and finally the init `moov` into `free` boxes. Result: `ftyp, free, (free,
  mdat)…, moov`, a plain MP4 with the index at the end; no data is copied. Order is the
  crash-safety argument: the appended `moov` is fsynced before any type changes; until
  the init `moov` becomes `free` (the last write) fragment-aware readers still play the
  file (ffmpeg skips a duplicate `moov`); the scanner recognises `free`d fragments, so an
  interrupted finalise is run again and produces the identical file (tested for every
  intermediate state). A torn `moov` at the end is cut off like a partial fragment.
- **Scanner** (`klip_mp4::scan_fragmented`): reads our files back (init `moov` →
  `TrackInfo`s, `moof`/`free`d fragments → sample index). Input is hostile: sizes are
  checked against the file and their parent box, allocations capped, and scanning stops
  without error at the first fragment that is incomplete or inconsistent, keeping
  everything before it. Tested with truncation at every byte of the last fragments and
  3000 mutated files.
- **Bookmarks** live on the capture timeline in the recorder. A session file gets those
  made while it was written; a clip gets those inside its range. Both are written as a
  Nero-style chapter list (`moov/udta/chpl`, version 1, 100 ns units, at most 255),
  which ffmpeg/ffprobe, VLC and mpv show as chapters; titles are "Bookmark n". Our editor
  reads the same box. A QuickTime chapter track (needs a text track and samples) and
  Adobe XMP markers were not needed for this.
- **Sidecar**: next to a file being written sits `<file>.recording.json`, written
  atomically (temp + rename) when the file starts and on every bookmark. It marks the
  file as unfinished and holds its bookmarks. Finalising deletes it. At `record` start,
  leftover sidecars in the recording folder are finalised (`recover_recordings`): files
  with no complete fragment are removed (nothing playable), files touched in the last
  15 s are skipped (another recorder may still write them), errors leave both files for
  the next try. `klip-cli finalize FILE` does the same for one file.
- **Feeding the writer**: the ring sink passes every packet the ring accepts to the
  session's tap while holding the ring lock (the tap only queues on a channel), so the
  recording sees exactly the ring's packets in order. A session starts with what the
  ring holds from its newest video keyframe on (up to one GOP before the request). The
  writer runs on its own thread; disk I/O never blocks capture threads.
- **Stopping** notes the video end, gives audio up to 250 ms to reach it (as clip saves
  do), detaches the tap and cuts every track at that point.
- **Limits**: recording stops with an error when free space on its drive drops below
  2 GiB (checked at start and every 5 s; the final `moov` must still fit) or when more
  than 512 MiB wait for the writer (the disk can't keep up). The file written so far is
  finalised. If a track description changes mid-recording (encoder restarted with new
  parameters), the file is finalised and recording continues in a new file.
- **Control**: commands `session.start`, `session.stop`, `session.toggle`,
  `bookmark.add`; `recorder.status` has a `session` block. Windows hotkeys come with the
  app (M6); on Linux the CLI binds toggle and bookmark through the GlobalShortcuts portal.

## Consequences
- A killed recorder leaves a file that plays in fragment-aware players (ffmpeg-based
  players, VLC, browsers, Windows' Media Foundation) without Klip; the next start turns
  it into a regular MP4. Measured on the dev PC (Windows 11, RTX 4080 SUPER, NVENC):
  killed after 10 s → 540 continuous frames with audio; after finalising the same frames
  with both bookmarks as chapters within 1 ms.
- Finished recordings have their index at the end (not faststart). Fine for local files
  and editors; sharing re-exports through the editor anyway.
- Editors that ignore `chpl` (most NLEs) won't show the bookmarks; Klip's editor will.
- Finalising reads every `moof` (a few KB per second of recording) and writes the index
  (about 10 MB per hour for 60 fps + 3 audio tracks at 12 bytes per sample, less with
  chunks per `trun`); no media data is copied.
