# klip-export

The export pipeline (ADR 0017): a `klip_edit::Render` plan + a preset → an MP4 next to
the clip. GStreamer crate in the engine layer (below `klip-engine`, above
`klip-capture`); the engine's media worker runs it in a child process (M8.4).

## How an export runs (`export(&ExportRequest, progress, cancel)`)

1. `Render::validate`, then `klip_mp4::probe` of the source (size, fps, codec, bitrate
   from bytes/duration, audio track names). The plan must be for this file's size.
2. The preset picks the output (`Output`): size (the crop, never scaled up), rate,
   codec, bitrate; `Original` = source size/rate/codec/bitrate (2–200 Mbit/s),
   `YouTube` ≤ 1920x1080 at ≤ 60 fps, 20 Mbit/s, source codec, `TikTok` fits
   1080x1920, 12 Mbit/s H.264, `Discord` H.264 + AAC 128 kbit/s with the bitrate from
   `fit::first_pass` (94 % of the limit minus audio; under 1.5 Mbit/s → 720p30, under
   0.7 → 480p30) and the encoder's peak = target. An unavailable codec falls back to
   H.264 (`Encoders` holds the ranked choices per codec, as names across processes).
3. One pass (`run_pass`): the text layers are rasterised once at the output size
   (`klip_edit::render_layer`); `source::Source` opens the clip (`filesrc → qtdemux`, a
   `decodebin` per stream; video → `videocrop` → `videoconvertscale` → BGRA appsink at
   the output size; every audio track → `audioresample` → `scaletempo` → S16 48 kHz
   stereo appsink); `encode::Encoder` starts the recorder's chain
   (`klip_capture::build_encode_chain`, non-leaky queue, the first encoder that starts)
   and the AAC chain. Per segment: a flushing accurate seek with the segment's rate;
   output frame `i` (time `i*1000/fps` ms, floor) shows the decoded frame with the
   largest pts ≤ `Render::frame_source_ms(i)`, the same integer mapping as the preview
   (0.5x repeats frames, 2x skips every other); a frame with active text cues is copied
   and blended on the CPU (`compose::blend_rgba_over_bgra`), others are pushed as they
   are. The segment's audio (tempo already changed by `scaletempo`) is cut or padded to
   the segment's output length. At the end the tracks are mixed (`Game`/`System` ×
   game volume + `Microphone`/`Voice` × mic volume, or `Mix` × game volume when the
   clip has no separate tracks), pushed through the AAC encoder, and the packets are
   muxed in memory (`klip_mp4::write_mp4`, faststart, bookmarks as chapters on the
   output timeline).
4. Discord: over the limit → `fit::next_pass` (bitrate × limit/actual × 0.97, at least
   5 % lower) and another pass; at most 3. Then the file is written through `.part`
   + rename (never overwrites).

Progress: `Progress { pass, frames_done, frames_total }` every 15 frames. Cancel: an
`AtomicBool` checked per frame; `Err(Cancelled)`, nothing on disk. Errors name the
step (`Plan`, `Source`, `NoEncoder`, `Encoder`, `TooLarge`, `Mp4`, `Io`).

Limits and guards: the plan must match the probed size and not reach past the clip's
length; at most 4 hours and an estimated 4 GB (`MAX_EXPORT_MS`, `MAX_EXPORT_BYTES`:
the output is held in memory until written); `appsrc` pushes never block (they wait
for room while watching the bus, 30 s), a pipeline that fails to start goes to NULL
(no leaked encoder session), a `.part` left by another export is never deleted; a
segment past the last video frame holds the previous frame; each segment is played
100 ms past its end so `scaletempo` drains, and a track that starts late is padded in
front; audio is pushed late by the AAC encoder's delay so packets start at 0.

Decoding is software by default when run in the media worker
(`prefer_software_decoders`), on the GPU otherwise; scaling is CPU (`videoconvertscale`,
all threads). GPU compositing (`d3d11compositor`) is a later optimisation.

## Tests

- Unit: presets → outputs, fit math, mixing, blending, plan safety
  (`hostile_plans_fail_cleanly`: bad plans, missing/junk files, an existing output).
- `tests/export.rs` (`#[ignore]`, `cargo xtask e2e`): a barcoded 720p60 clip with
  `Game` and `Microphone` beep tracks made with the machine's H.264 encoder, then:
  trim + cut + 2x + 0.5x → every output frame's counter equals the plan's source
  frame, 60 fps, 8 s, one Mix track, faststart; a text cue → the overlay region matches
  the bitmap composited over the source frame (PSNR ≥ 40 dB) and is absent outside
  the cue; Discord on 5, 12, 25 (with a forced second pass), 40 and 60 s clips → ≤
  10 MB, h264 + aac, every frame readable and in order; cancel → no file, no `.part`;
  volumes → only the chosen track's beeps, and 2x halves their times.
