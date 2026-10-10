# 0018: Editor (Advanced mode): multi-track model, keyframes, montage and the compositing path

Status: proposed (M9.1, 2026-10-10; to be confirmed at the M9 wrap-up). Author: Claude
Fable 5.1.

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
- **Spike first (M9.1).** Before building on option 2: a measured check that
  `compositor`/`d3d11compositor` with two 1080p60 sources, keyframed zoom and a
  crossfade sustains ≥ 2x realtime on the dev machine from the media worker. If it
  doesn't, option 1 is taken for Advanced mode only, with the parity test unchanged.
- **Custom export** is `Preset::Custom {width, height, fps, bitrate_kbps, codec}` in
  `klip-export`, validated like the recording's custom quality.

## Consequences
- Normal mode stays as fast and simple as it is; the Advanced UI is a second workspace
  layout (`src/editor/advanced/*`) over the same store, with its own layout
  rearranging (the brief allows it only there).
- Music, stickers and sound effects are user files from the library folder or chosen
  with a file picker; every asset shipped with Klip is attributed (rule 5).
- Word captions wait for the captions milestone ("Later"); the schema reserves
  `captions` for them.
- The M8 in-memory muxing cap matters more for montages: IMPROVEMENTS G1 (streaming
  the MP4 to disk) is pulled into M9.3.
