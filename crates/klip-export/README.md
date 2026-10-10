# klip-export

The export pipeline (ADR 0017, ADR 0018): a `klip_edit::Composition` + a preset → an MP4
next to the clip. GStreamer crate in the engine layer (below `klip-engine`, above
`klip-capture`); the engine's media worker runs it in a child process (M8.4).

## How an export runs (`export(&ExportRequest, progress, cancel)`)

The request carries a `klip_edit::Composition` (`compose(project, source_info)`: a
project in either mode as layers on the output timeline; Normal mode is one track of
back-to-back video layers) and the files of its other sources (`media`: library clips,
music, sounds, pictures, as the engine resolved them).

1. Cheap checks first (empty or > 4 h, a crop outside the clip), then `klip_mp4::probe`
   of the main clip and of every other file (a missing one fails by name: "the clip X
   is missing"; never dropped), then `check_plan`: size, crop, length, caps on layers,
   keyframes and texts, at most `render::MAX_RUNNING` (16) layers playing at once, an
   estimate of the output size (64 GB sanity bound).
2. The preset picks the output (`Output`) from the main clip's crop: size (never scaled
   up), rate, codec, bitrate; `Original` = source size/rate/codec/bitrate (2–200
   Mbit/s), `YouTube` ≤ 1920x1080 at ≤ 60 fps, 20 Mbit/s, source codec, `TikTok` fits
   1080x1920, 12 Mbit/s H.264, `Discord` H.264 + AAC 128 kbit/s with the bitrate from
   `fit::first_pass` (94 % of the limit minus audio; under 1.5 Mbit/s → 720p30, under
   0.7 → 480p30) and the encoder's peak = target, `Custom {width, height, fps,
   bitrate_kbps, codec}` = the crop fitted into that box up or down (≤ 4096), 1–240 fps,
   0.1–200 Mbit/s. An unavailable codec falls back to H.264 (`Encoders` holds the
   ranked choices per codec, as names across processes).
3. One pass (`render::run_pass`), frame by frame:
   - Every layer plays in pieces (`Layer::pieces`, ≤ 60 s): one flushing, accurate seek
     of its `source::Source` per piece at the piece's rate, like a Normal-mode segment.
     A `Source` is `filesrc → decodebin`; video → `videocrop` (the main clip's format
     crop) → `videoconvertscale` → BGRA appsink at the layer's decode size (fitted size
     × its largest zoom, never above the file's own size; the main clip without zoom
     at exactly the output size); every audio track → `audioresample` → `scaletempo` →
     S16 48 kHz stereo appsink. Music and sound layers open without video (the stream
     stays undecoded); pictures decode once at their own size (≤ 4096 a side). Sources
     are pooled: layers that follow each other in one file at one size share a
     pipeline (Normal mode is one source), two layers playing at once never do.
   - Output frame `i` (time `i*1000/fps` ms, floor) shows, per visible layer (bottom
     first: track order, then id), the decoded frame with the largest pts ≤
     `layer.source_ms(t)`. When the frame is a single opaque layer filling the canvas
     at the output size (Normal mode), it is pushed as it is; otherwise `canvas::Canvas`
     composes it (only the visible part of each layer is cropped and scaled by a
     `GstVideoConverter`, straight into the canvas for opaque video, blended with alpha
     × opacity otherwise; then the colour matrix). Text cues are rasterised once per pass
     and blended on top on the CPU (`compose::blend_rgba_over_bgra`).
   - A piece's audio is taken when it ends (`Source::drain_audio`), weighed per track
     (`Game`/`System` × game volume + `Microphone`/`Voice` × mic volume for the main
     clip, or `Mix` when there are no separate tracks; other clips at 1), multiplied by
     the layer's volume keyframes per ms, and added to the music or the main bus of the
     `mixer::Mixer`; microphone tracks and sound/voice-over layers also feed the ducking
     detector (10 ms RMS gate at −45 dBFS → the music goes to `duck_db`, one-pole
     attack/release). Audio is handed to the AAC encoder up to the start of the earliest
     piece still playing, so memory is bounded by one piece, not by the export.
   - Encoding: the recorder's chain (`klip_capture::build_encode_chain`, non-leaky
     queue, the first encoder that starts) and the AAC chain; packet bytes go to
     `<out>.spool` as they come (only `klip_mp4::Sample` metadata in memory), and
     `klip_mp4::write_mp4_spooled` writes `<out>.part` (faststart, the composition's
     markers as chapters) at the end of the pass.
4. Discord: over the limit → `fit::next_pass` (bitrate × limit/actual × 0.97, at least
   5 % lower) and another pass; at most 3. Then `.part` is renamed (never overwrites).

Progress: `Progress { pass, frames_done, frames_total }` every 15 frames. Cancel: an
`AtomicBool` checked per frame; `Err(Cancelled)`, nothing on disk. Errors name the
step (`Plan`, `Source`, `Missing`, `NoEncoder`, `Encoder`, `TooLarge`, `Mp4`, `Io`).

Limits and guards: `appsrc` pushes never block (they wait for room while watching the
bus, 30 s), a pipeline that fails to start goes to NULL (no leaked encoder session), a
`.part` or `.spool` left by another export is never touched (`files::OwnedFile`: made
with `create_new`, removed unless committed); a piece past the last video frame holds
the previous frame; each piece is played 100 ms past its end so `scaletempo` drains,
and a track that starts late is padded in front; audio is pushed late by the AAC
encoder's delay so packets start at 0.

Decoding is software by default when run in the media worker
(`prefer_software_decoders`), on the GPU otherwise; scaling and composing are CPU
(`videoconvertscale` and `GstVideoConverter` on all threads, blending and the colour
matrix on row bands). `d3d11compositor` is a later optimisation.

## One frame (`frame::frame`, `editor.frame`, M9.4)

`frame(&FrameRequest {source, composition, media, out_ms, width})` → `FrameImage
{width, height, out_ms, jpeg}`: the picture the export encodes at `out_ms` (clamped to
the last frame), at `width` (16…4096; the height follows the format, as a Custom export
of that width). Only the layers on screen then are read (a montage clip missing
elsewhere doesn't matter); each is opened and accurately seeked to its source time and
the frame the frame loop would pick is kept (largest pts ≤ the time; the first frame
when none is; the file's last frame when the time is past its end). The picture is
drawn by `render::Drawer`, the step the export's frame loop uses (placement, opacity,
colour matrix, text cues), and encoded as a full-range YCbCr JPEG
(`klip_capture::thumbnail::encode_bgra_jpeg`, quality 92). `render::source_config` and
`native_size` are shared the same way.

## Tests

- Unit: presets → outputs (Custom included), fit math, plan checks (caps, crop,
  layers at once, a missing file named), the canvas (crop + scale of the visible part,
  stacking with alpha and opacity, black where nothing is, the colour matrix), the mixer
  (weights, per-ms volume keys, whole-block flushes, ducking −12 dB under a voice and
  back), `files::OwnedFile`; `hostile_plans_fail_cleanly` (bad plans, missing/junk
  files, an existing output).
- `tests/export.rs` (`#[ignore]`, `cargo xtask e2e`): a barcoded 720p60 clip with
  `Game` and `Microphone` beep tracks made with the machine's H.264 encoder, then:
  trim + cut + 2x + 0.5x → every output frame's counter equals the plan's source
  frame, 60 fps, 8 s, one Mix track, faststart; a text cue → the overlay region matches
  the bitmap composited over the source frame (PSNR ≥ 40 dB) and is absent outside
  the cue; Discord on 5, 12, 25 (with a forced second pass), 40 and 60 s clips → ≤
  10 MB, h264 + aac, every frame readable and in order; cancel → no file, no `.part`;
  volumes → only the chosen track's beeps, and 2x halves their times.
- M9.3 in the same file: two barcoded clips montaged with a crossfade → every frame
  outside the 500 ms blend shows the plan's clip and source frame; a 2x zoom → the
  output matches the middle half of the source scaled up (PSNR, and far from the
  unzoomed frame); brightness +0.2 → +51 levels; music ducks −12 dB (±1.5) under a voice
  layer (Goertzel on the music tone), and not without ducking; Custom 640x640 box, 30
  fps, 1.5 Mbit/s, H.264 → 640x360, 30 fps, h264, the bitrate; a 10-minute montage (nine
  crossfaded library clips, zooms, music) exports with the test process's working set
  under 1 GB.
