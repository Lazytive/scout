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
  height, weight, fill, outline, background, default position, uppercase);
  `clean_text`. Rendering to bitmaps is M8.2.
- `template`: `Shorts` = tall centred + the clip's name as a caption for the first 3 s.

Tests: unit tests per module; `tests/hostile.rs` (proptest: any JSON loads, repair is
idempotent, random edits keep the timeline consistent and invertible within 1 ms).
