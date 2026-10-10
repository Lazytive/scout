# 0017: Editor (Normal mode): project model, preview, export pipeline and presets

Status: proposed (M8.1, 2026-10-09; to be confirmed at the M8 wrap-up). Author: Claude
Fable 5.1.

## Context
M8 adds the editor's Normal mode (KLIP_PROMPT §3 "Editor"): non-destructive edits on one
clip (trim, split and cut, text in preset styles, 16:9 / 9:16 format, 0.5x / 1x / 2x
speed on a selection, game and mic volume, templates such as "Shorts") and export
presets (Discord that always fits the upload limit, YouTube, TikTok/Shorts, Original).
The brief requires that the preview matches the export, with a parity test, and that
the original clip is never changed. The decisions owed since M7 (STATUS): GStreamer
Editing Services (GES) vs an own pipeline; how the preview plays clips given that
WebView2 cannot play HEVC without the paid extension; the parity test; the Discord
two-pass bitrate.

Facts checked on this machine (GStreamer 1.28.6 MSVC): the `ges` and `nle` plugins are
installed; `decodebin3`, `videocrop`, `videoconvertscale`, `d3d11convert`,
`compositor`, `d3d11compositor`, `textoverlay`, `scaletempo`, `volume`, `audiomixer`
are available. WebView2 (Chromium) plays H.264 + AAC and AV1 (dav1d) in `<video>`; HEVC
depends on the Windows HEVC extension. The UI uses system fonts (ATTRIBUTION: no bundled
font files), so text drawn by the WebView would differ from machine to machine.

## Options
1. **GES** for the export timeline. Pro: clips, layers, effects and time-stretch exist.
   Con: a second, large API surface (gstreamer-editing-services bindings) whose
   behaviour we don't control, no D3D11 memory path, its own text rendering (pango)
   that the WebView cannot reproduce, and the Normal mode needs five operations only.
2. **Own pipeline (chosen)**: per output segment a `decodebin3` branch with a precise
   seek, our own timestamp mapping (cuts, speed), `videocrop`/`videoconvertscale` (or
   `d3d11convert`), `compositor` for the text bitmaps, `volume` + `audiomixer` +
   `scaletempo` for audio, then the existing `klip_capture::EncodeChain` (the same
   ranked hardware encoders as recording) and `klip_mp4::write_mp4` (faststart, named
   tracks, markers). Everything Klip already owns; GES stays an option for M9's montage.

## Decision
- **Crates.** `klip-edit` (pure: no GStreamer) holds the project model, the timeline
  math and the text layout/rasterisation; `klip-export` (GStreamer) builds and runs the
  export pipeline from a `klip_edit::Render` plan. The engine exposes commands; the
  Tauri app is a thin shell as before (ADR 0014).
- **Project file.** One JSON file per clip, `.klip/projects/<clip key>.json` (keyed and
  renamed like thumbnails, ADR 0015), schema v1 with `deny_unknown_fields` off and
  lenient load (bad fields → default + warning; never fail), atomic save. It holds:
  `cuts` (kept source ranges, ms), `speed` runs over kept ranges (0.5 / 1 / 2), `format`
  (`wide` 16:9 or `tall` 9:16 with a crop x-position 0..1), `text` layers (preset id,
  text ≤ 200 chars, position preset, start/end in output time), `volumes` (game, mic
  0..2) and the `mode` toggle. Advanced-mode fields come in M9 under their own keys;
  Normal mode keeps unknown keys it doesn't show (the "advanced edits" badge).
- **Timeline math lives once, with shared golden fixtures.** `klip_edit::timeline` maps
  output time ↔ source time (cuts, speed) and lists the output segments; the front end
  has the same functions in TypeScript. Both are checked against the same fixture files
  (`crates/klip-edit/fixtures/*.json`: project + expected mappings), by `cargo test`
  and by vitest. That is half of the parity test: the preview seeks where the export
  cuts.
- **Preview plays the real clip** in a `<video>` through Tauri's `asset:` protocol
  (feature `protocol-asset`; Range requests supported by Tauri), with the clip's path
  allowed in the asset scope when the editor opens it and removed when it closes
  (never the whole folder). Edits are previewed in the WebView: `currentTime` follows
  the timeline math, speed uses `playbackRate`, the 9:16 format is a CSS crop, text
  layers are the **same PNG bitmaps the export composites** (served by the `klipthumb`-
  style scheme from `.klip/render/`), volumes set the `<audio>` gains of the separate
  tracks. If the `<video>` reports an error (HEVC without the extension, or an odd
  file), the engine makes an H.264 720p **proxy** in the media worker
  (`.klip/proxies/<key>.mp4`) and the preview uses that; export always reads the
  original.
- **Text is rendered by Klip, not by the browser.** `klip_edit::text` lays out and
  rasterises each text layer to an RGBA PNG at the output size with a **bundled
  open-licence font** (one family, OFL; entry in ATTRIBUTION.md; the UI itself keeps
  system fonts). Presets are data (font size as a fraction of height, fill, outline,
  pill background, shadow, position). Preview and export show the identical pixels;
  the second half of the parity test compares the overlay region of an exported frame
  to the PNG. (M8.4: the project file lives at `.klip/projects/<key>.json` beside the
  thumbnails, with proxies in `.klip/proxies/` and renders in `.klip/render/`, all
  following rename/delete and pruned together; the preview's file is handed to the
  WebView through Tauri's asset protocol with a scope of exactly one engine-named
  file at a time.) (M8.2: the font is Inter 4.1 Bold/ExtraBold, outlines through `skrifa`
  and coverage through `ab_glyph_rasterizer`, no shaping; the bitmap is cropped to the drawn area and carries its frame position, so
  the compositor and the preview place it at `x`/`y` instead of blending a full
  frame; `cache_key` names the cached PNG.)
- **Export runs in the media worker** (M8.3: `klip-export` builds the pipeline from a
  `klip_edit::Render` plan — the crop, the output segments, the text cues, the
  volumes and the bookmarks as markers — computed once from the project and the
  probed clip; decoding/cropping/scaling in one `decodebin` pipeline seeked per
  segment with the segment's rate, `scaletempo` for audio, frames picked by the
  plan's integer mapping, text blended on the CPU, the recorder's encoder chain and
  AAC chain with a non-leaky queue, `write_mp4` in memory, then `.part` + rename)
  (ADR 0015/0016 pattern: `--klip-media-worker`,
  JSON lines, timeout, kill) as an `ExportJob` with progress lines and cancel; a
  decoder or encoder crash never reaches the app or the replay buffer. Exports are
  written atomically next to the clip as `<name> - <preset>.mp4` (never overwriting,
  `klip_library::names` rules), so they appear in the Library. Audio: the clip's Game
  and Microphone tracks (or Mix alone when they're absent) → `volume` → `audiomixer`
  → one AAC Mix track; speed changes keep pitch (`scaletempo`).
- **Presets.** Original (source size and fps, source bitrate), YouTube (≤ 1080p or
  source, 60 fps, 20 Mbit/s H.264 or source codec), TikTok/Shorts (1080x1920, 60 fps,
  12 Mbit/s H.264), Discord (H.264 + AAC 128 kbit/s, fits `export.discord_limit_mb`,
  default 10 MB). **Discord fit**: pass 1 encodes at `(0.94 x limit - audio bytes) /
  duration` with the encoder's max-bitrate = target; if the file is over the limit,
  pass 2 re-encodes at `target x limit / actual x 0.97`; if the target falls under
  1.5 Mbit/s the resolution steps down (1080p → 720p → 480p) and fps to 30 before
  encoding. Acceptance: the result is never over the limit on the test clips.
- **Templates** are project presets applied in one step: "Shorts" = `tall` format with
  the crop centred, a caption-style text layer with the clip name, speed 1x.
  Captions/zoom-on-action wait for later milestones.

## Consequences
- Normal mode has no dependency on GES; M9 (Advanced: multi-track, keyframes, montage)
  re-evaluates GES against extending `klip-export`.
- One bundled font (licence + attribution) and one more cache folder (`.klip/render`,
  `.klip/proxies`), both rebuildable, pruned with the thumbnails.
- The parity test is two tests (timeline fixtures shared by Rust and TypeScript; the
  overlay bitmap compared in an exported frame) plus the e2e that decodes an export of a
  barcoded clip and checks the frame sequence against the timeline math.
- Discord exports may take two encodes; the second is rare (hardware encoders hold their
  max-bitrate well) and the worker reports both passes in the progress.
