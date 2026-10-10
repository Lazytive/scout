# klip-edit

The editor's model (ADR 0017). Pure: no media, no GStreamer, no OS calls besides reading
and writing the project file.

- `Project` (schema v1): `source_ms` (the clip's length), `pieces` (kept source ranges
  in order, each with a `speed` of 0.5 / 1 / 2; empty = the whole clip), `format`
  (`wide` or `tall` 9:16 with `crop_x` 0..1), `text` layers (preset style, text, position,
  start/end in **output** time), `volumes` (game, mic: 0..2), `mode` (normal /
  advanced). Unknown keys are kept in `extra` (`has_advanced_edits()`): Advanced mode
  (M9) and newer versions never lose work when Normal mode saves.
- `load(path)` never fails: a non-object, invalid JSON/UTF-8 or a file over 1 MiB → a new
  project (`LoadStatus::Reset`); wrong fields → defaults, bad list entries dropped,
  then `repair` (clamp to the clip, sort pieces, drop overlaps and empty ranges, clean
  text, unknown presets → `bold`, duplicate ids renumbered, volumes and crop clamped)
  with an `Issue { path, message }` per change (`LoadStatus::Repaired`); repaired and
  unusable files are copied to `<file>.bad`. Caps: 256 pieces, 64 text layers, 200
  characters and 3 lines per text, 64 unknown keys, 24 h. `save` = temp + fsync + rename.
- `timeline`: `segments` (pieces placed back to back on the output timeline),
  `output_duration_ms`, `source_time(out)` ↔ `output_time(src)`, and the Normal-mode
  edits in output time: `split`, `trim`, `cut`, `set_speed`. Integer math: a piece at
  speed num/den has output length `ceil(len·den/num)`; output offset `o` shows source
  offset `floor(o·num/den)` (0.5x repeats each ms twice, 2x skips every second one).
  A refused edit (`EditError`) leaves the project as it was.
- `fixtures/*.json`: projects with their expected segments and time mappings. Checked
  here (`tests/fixtures.rs`) and by the front end's vitest against its TypeScript port
  (`apps/klip-app/src/lib/timeline.ts`, M8.5): the preview seeks where the export cuts.
- `format`: `crop_rect(w, h)` (tall: full height, width `h·9/16`, x from `crop_x`; even
  sizes) and `output_size(w, h, max_height)` (never scales up).
- `text`: presets `bold`, `pill`, `caption`, `title` as data (size as a fraction of the
  height, weight, fill, outline, pill background, shadow, default position, uppercase);
  `clean_text`.
- `text::render` (M8.2): `render(text, style, position, w, h)` → `TextBitmap`
  (straight-alpha RGBA cropped to the drawn area, with its `x`/`y` in the frame; never
  empty; `png()`), `render_layer`, `measure` (font size after auto-shrink, the wrapped
  lines, block top, `shrunk`/`truncated`), `cache_key` (FNV-1a 64 of renderer version,
  font, frame size, position, style and cleaned text: the file name under
  `.klip/render/`). Font: Inter 4.1 Bold and ExtraBold (OFL, `assets/fonts/`,
  `include_bytes!`), outlines through `skrifa` (Google's fontations, unhinted) and
  coverage through `ab_glyph_rasterizer` (no shaping or kerning: RTL scripts come out
  in logical order, ligatures as separate letters, emoji and other missing glyphs as
  the font's "no glyph" box; joiners, marks and bidi controls are skipped). Layout:
  word wrap inside 5 % side margins (runs of spaces collapse; a wider word breaks
  between characters), at most 3 lines, shrink by 10 % per step until the lines fit
  the width and the block fits the height, down to 4 px, then cut; 6 % top/bottom
  margin, lines centred, line height 1.2 × size, whole-pixel positions. Style values
  are clamped (size ≤ 0.5 of the height, outline ≤ 0.3 of the size, radii ≤ 32 px). Effects back to front: pill per line
  (radius = half the line), shadow (offset copy of the coverage, two box blurs),
  outline (coverage dilated by a disc, only edge pixels splat), fill; premultiplied
  compositing, unpremultiplied at the end. Sides up to 4096 px. Changing any of this
  changes the golden hashes in `tests/text_render.rs`: bump `RENDER_VERSION`.
- `template`: `Shorts` = tall centred + the clip's name as a caption for the first 3 s.
- `plan` (M8.3): `Render::new(&Project, &SourceInfo)` = what `klip-export` renders:
  the crop (`format::crop_rect` on the probed size), the output segments
  (`timeline::segments` after clamping the project to the clip's length), the text
  layers cleaned and clipped to the output, the volumes, and the clip's bookmarks mapped
  to output time as `Marker`s (cut ones dropped, at most 255). `validate` (crop inside
  the source, segments consistent, positive length), `frame_out_ms(i, fps)` =
  `i*1000/fps` floor, `frame_source_ms(i, fps)` (the frame the export shows: the same
  integer math as the preview), `frame_count(fps)`, `texts_at(out_ms)`. Serde, lenient.

Tests: unit tests per module; `tests/hostile.rs` (proptest: any JSON loads, repair is
idempotent, random edits keep the timeline consistent and invertible within 1 ms);
`tests/text_render.rs` (golden hashes of the four presets at 1920x1080 and 1080x1920,
hostile strings and random strings/bytes/frames never panic and stay inside the frame;
`KLIP_TEXT_DUMP=<dir>` writes the renders over grey for a look).
