# 0018: Editor (Advanced mode): multi-track model, keyframes, montage and the compositing path

Status: accepted (M9.6, 2026-10-10), with the amendments below (M9.3–M9.6): where an
earlier bullet and an amendment differ, the amendment holds. Proposed in M9.1 by Claude
Fable 5.1; accepted by Claude Opus 5.5.

## Context
Normal mode (ADR 0017) edits one clip with pieces, a format crop, text presets and two
volumes; `klip-export` renders it frame by frame from a `Render` plan and the preview
plays the clip itself along the output timeline. The brief (KLIP_PROMPT §3) asks
Advanced mode for a multi-track timeline (video, text, mic, game audio, music with
auto-ducking), keyframes for zoom, position and volume, speed ramps, free crop and
position with numbers, basic colour, transitions, stickers and sound effects, word
captions once captions exist, a montage of several clips, markers from bookmarks and
auto-clip events, J/K/L, snapping, undo history, and a custom export. Switching modes
must never lose work: Advanced edits stay in Normal mode behind an "advanced edits"
badge, and Normal edits stay visible in Advanced mode.

Constraints that carry over: no GPL (GES itself is LGPL and dynamically linked, so it
is allowed; its `nle` plugin too), preview/export parity (the export must show what the
preview showed), every input hostile, exports in the media worker, no shaping/kerning
in text yet (M8.2), the in-memory muxing cap (IMPROVEMENTS G1).

## Options
1. **GStreamer Editing Services** for Advanced mode's timeline and export. Pro: layers,
   clips, transitions, effects and time-stretch exist; a timeline → pipeline in one
   call. Con: a second large API surface whose behaviour we don't control; its own text
   rendering; no D3D11 path (uploads/downloads around every effect); the Normal-mode
   export would have to move too or two renderers would have to agree; preview parity
   would mean running GES in the preview as well.
2. **Extend `klip-export` into a compositor** (chosen, pending the M9.1 spike): a
   `Composition` plan (tracks of placed clips, each with keyframed transform, opacity,
   volume and speed, plus transitions and overlays) rendered output-frame by
   output-frame: one decoder pipeline per source clip (seeked as today), frames
   composed by GStreamer's `compositor` (or `d3d11compositor` where it negotiates) with
   per-pad properties set from the keyframes, audio through `audiomixer` with a
   `volume` per track driven by the ducking envelope, then the existing encode chain.
   Everything Klip already owns; Normal mode's plan becomes a one-track composition.
3. Hybrid: GES only for montage (clip sequencing) and our compositor for the rest.
   Two renderers to keep in sync: rejected.

## Decision
- **Schema v2 on the same file.** The project keeps `version: 1` fields exactly as they
  are (Normal mode reads and writes them) and gains an `advanced` object with its own
  keys: `tracks` (video, text, audio, music; each a list of placed clips with a source
  reference, in/out, position on the timeline, keyframed `transform` {zoom, x, y},
  `opacity`, `volume`, `speed` ramps), `transitions`, `overlays` (stickers), `colour`
  {brightness, contrast, saturation}, `markers`, `ducking` {music under voice}. Normal
  mode's `pieces`, `format`, `text`, `volumes` remain the source of truth for the main
  clip and are projected into the composition when rendering; Advanced edits that
  Normal mode can't show set the badge (`has_advanced_edits`). A montage project lives
  under the first clip's key and references the others by library key.
- **One renderer.** `klip_edit::compose` turns a project (both modes) into a
  `Composition`; `klip-export` renders a `Composition` (the M8 `Render` becomes its
  single-track case). Keyframes are evaluated per output frame in integer ms with the
  same functions in Rust and TypeScript, checked by shared fixtures as in M8.
- **Preview.** Playback stays `<video>` per clip for the main track (parity for cuts
  and speed as in M8); the composed picture (zoom, position, colour, overlays,
  transitions) is drawn by the page from the same keyframe math on a canvas over the
  video, and a `editor.frame` command renders any single output frame through the real
  compositor for scrubbing and for the parity test (preview canvas vs engine frame,
  PSNR ≥ 35 dB on synthetic content).
- **Spike first (M9.1): done, option 2 confirmed.** `diag_compositor_spike`
  (`crates/klip-export/tests/export.rs`) composes two 1080p60 clips, the second zoomed
  1x → 2x by a per-frame keyframe and crossfaded in, into the NVENC chain: `compositor`
  (system memory) 4.6x realtime, `d3d11compositor` 8.4x, on the dev machine
  (2026-10-10, i7-14700KF + RTX; software decode). Both clear the 2x bar, so Advanced
  mode extends `klip-export`; `d3d11compositor` is the first choice on Windows with
  `compositor` as the fallback (and Linux's path).
- **Composing in-process, not in the `compositor` element (M9.3, 2026-10-10).** The
  renderer composes each output frame itself: for every visible layer only the part
  that lands on the canvas (`klip_edit::compose::visible`) is cropped and scaled by a
  `GstVideoConverter`, straight into the canvas for opaque video, or into a scratch
  buffer blended with alpha × opacity; the colour grade is one affine RGB matrix
  (`Colour::matrix`, the same numbers an SVG `feColorMatrix` takes in the preview).
  Why: the `compositor` pads would scale the whole input to the placed size before
  clipping (an 8x zoom of 1080p is a 15360x8640 intermediate per frame), its pad
  properties must be synchronised with buffers (control sources or lock-step pushing),
  and a pure "frames + numbers → pixels" step is deterministic and unit-testable
  without pipelines, which `editor.frame` and the preview parity test need. Sources
  stay GStreamer pipelines (one per playing layer, pooled), encoding stays the
  recorder's chain. `d3d11compositor` remains a later optimisation.
- **Custom export** is `Preset::Custom {width, height, fps, bitrate_kbps, codec}` in
  `klip-export`: the width x height is a box the edit's format is fitted into (up or
  down, ≤ 4096), so the format decides the shape and the export only the resolution.
- **`editor.frame` (M9.4, 2026-10-10)** draws with the export's own per-frame step
  (`render::Drawer`, shared by the frame loop) after opening and accurately seeking each
  clip on screen, in a media worker of its own, and returns a full-range YCbCr JPEG.
  Measured against the exported frames of a crossfade montage: 48–51 dB, barcodes as
  planned. It is for paused frames and the parity test, not for scrubbing (a call
  opens every clip on screen; 35–220 ms). A montage is a project under its first clip
  whose other clips are library keys; a clip that is gone is reported (`missing`) and
  kept, never dropped, and a renamed one is followed. Project history is file
  snapshots of the project a save replaces (at most one a minute, 30 kept).
- **The preview (M9.5, 2026-10-10)** is a canvas the front end draws with the shared
  math ported to TypeScript (`compose.ts`, `keyframes.ts`; Rust writes the expectations
  of `fixtures/advanced/{placement,compose}.json`, both sides check them), one media
  element per playing layer on a wall-clock master, the grade as an SVG
  `feColorMatrix` in sRGB, text cues as the engine's own bitmaps. Sound: a `<video>`
  plays only a file's first audio track, so the engine copies a clip's tracks one per
  file (`editor.audio`, remuxed, cached per clip) and Web Audio weighs them like the
  export's mixer (Normal mode's Game/Mic sliders included). The app lets the page read
  only a bounded set of files the engine named (`editor_media` with `what`). Measured
  in the app: canvas vs `editor.frame` 39.9–66.8 dB at 1280x720 (bar: 35 dB). A dip on
  an upper track fades to what is below it (black only on the lowest track).
  **Amended (M9.8.5, 2026-10-11):** a dip on a picture track goes through black, as in
  FilmCraft/Premiere: compose adds a `LayerKind::Black` layer (the whole frame, no file;
  the track's z, a later id, so over both clips and under higher tracks) whose opacity
  goes 0 → 1 at the cut (held across a gap) → 0, and leaves the clips' opacity alone;
  both renderers draw it (the export as a stretched opaque black frame). Sound tracks
  keep the old fades. Export test: mean luma 70.9 before, 35.3 half way, 0.0 at the cut.
  **Amended (M9.8.7–9, 2026-10-11):** tracks get `hidden` (no picture, sound stays) and
  `main`: V1 arranged on the Advanced timeline, a track of the main clip's pieces that
  replaces Normal mode's cut in `compose` (z 0; gaps are black and silent). Normal mode
  can't show it, so it is off while a project has one ("Back to the simple cut" removes
  it). Both fields default to false, so older projects read as before.
- **Accepted at the M9 wrap-up (M9.6, 2026-10-10)** on the e2e `m9_montage.rs`: two
  clips the daemon recorded of the test game, a montage through the engine's commands
  (Normal trim, `editor.montage`, a split with a 500 ms crossfade, a 2x zoom), a Custom
  export in the media worker: every plain frame is the source frame `compose_with`
  names (420/420), the crossfade's middle frame is the mix of its two source frames
  (43.6 dB) and a zoomed frame the doubled centre of its source frame (43.7 dB). One
  renderer for both modes and the shared math held on real recordings.

## Consequences
- Normal mode stays as fast and simple as it is; the Advanced UI is a second workspace
  layout (`src/editor/advanced/*`) over the same store, with its own layout
  rearranging (the brief allows it only there).
- Music, stickers and sound effects are user files from the library folder or chosen
  with a file picker; every asset shipped with Klip is attributed (rule 5).
- Word captions wait for the captions milestone ("Later"); the schema reserves
  `captions` for them.
- The M8 in-memory muxing cap matters more for montages: IMPROVEMENTS G1 (streaming
  the MP4 to disk) is pulled into M9.3 (done: packets spool to `<out>.spool`).
- Ducking keys on speech, not on everything that isn't music: microphone/voice tracks
  of clips and sound/voice-over layers duck the music; game audio does not (it would
  keep the music ducked all the time).
