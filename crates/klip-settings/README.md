# klip-settings

Scout's settings (ADR 0014). Pure: no OS calls besides reading and writing the file.

- `Settings`: the schema v1 with defaults (`app`, `clips`, `recording`, `audio`,
  `hotkeys`, `overlay`, `storage`, `appearance`, `games`, `export`). Every struct is
  `#[serde(default)]`, so missing fields take their defaults. `games.detect_stores`
  (M7, default true): store-library games count; off, only `user_games` (tests use it).
- `load(path)` never fails. Invalid JSON / UTF-8, a non-object or a file over 1 MiB →
  defaults (`LoadStatus::Reset`). Otherwise wrong fields are removed one at a time
  (located with `serde_path_to_error`; a bad list entry goes as a whole) and get their
  defaults, then `validate` clamps ranges, trims and deduplicates lists, drops invalid
  program names, normalises hotkeys and profile keys (`LoadStatus::Repaired`). Every
  repair is an `Issue { path, message }`; repaired and unusable files are copied to
  `<file>.bad`. Arrays over 256 entries and objects over 1024 keys are cut before
  repairing, and at most 512 repairs are tried, so hostile files stay cheap. Issue
  messages never repeat folder paths (they can contain the user's name).
- `save(path, &settings)`: temp file + fsync + rename.
- `Settings::apply_patch(json)`: RFC 7396 merge patch, strict: unknown keys, wrong types
  and out-of-range values are rejected with their path (`SettingsError::Invalid`), and
  nothing changes. `null` resets a field to its default or removes a profile; `version`
  can't be patched.
- `Settings::effective(exe)`: the global settings with that game's profile applied
  (profiles are keyed by `game_key(exe)`, the lower-case file name).
- `preset`: `Low` 720p30 8 Mbit/s, `Balanced` 1080p60 20 Mbit/s, `High` up to 1440p at
  the monitor's refresh clamped to 60–144 fps, 40 Mbit/s, `Custom`; presets use H.264.
- `appearance` (ADR 0019): `theme` = `system` (default: Light or Dark from the OS),
  `dark`, `graphite`, `light`, `oled` or `custom` (then `custom_theme` = a theme id;
  `custom` without a valid id becomes `system`); `accent` (`#rrggbb` or `null` = the
  theme's own; `#rgb` and upper case are normalised); `density` (`comfortable` default |
  `compact`); `text_size` (`small` | `default` | `large` | `larger`); `motion`
  (`system` default | `reduce` | `full`).
- `theme_file` (M10): custom theme files. `parse_export` (a shared `*.scout-theme`, picture
  embedded as a data URI, ≤ 12 MiB), `parse_stored` (`themes/<id>.json` ≤ 64 KiB, the
  picture named in `background.file`), `from_value` (the editor's edits); all repair
  field by field with issues and refuse only non-JSON, a missing/newer `scout_theme` (or `klip_theme`, before the rename)
  version or an oversized file. `Picture::from_bytes` checks PNG/JPEG/WebP magic bytes
  and ≤ 8 MiB. `to_export`/`to_stored`/`to_ui` write the three forms; `slug`/`valid_id`
  make and check ids (`[a-z0-9-]`, ≤ 40); `valid_font` keeps CSS syntax out of font
  names.
- `hotkey`: `Hotkey`/`Key`, parsed from `Ctrl+Alt+Shift+Win+Key` or the shortcuts-spec
  trigger format (`CTRL+ALT+SHIFT+LOGO+keysym`), case-insensitive, with aliases;
  `Display` gives the stored form, `to_trigger()` what `klip_platform::GlobalShortcuts`
  takes. Defaults: Alt+F10 clip 30 s, Alt+Shift+F10 clip 2 min, Alt+F9 recording,
  Alt+F8 bookmark.

Tests: theme files (round trips of all three forms, repairs, refusals, deep nesting,
picture magic, path-safe picture names, 2000 random JSON trees: parse or refuse, output
re-parses unchanged), repairs field by field and entry by entry, clamping, strict patches (paths,
unknown keys, `null`, profiles), profiles in `effective`, presets, every key × every
modifier set round-trips through both hotkey formats, file load/save/backup cases, a
hostile 200 000-entry file, and property tests (2000 random JSON trees each: loading
always gives settings that validate cleanly and round-trip; patches either apply
cleanly or are rejected; no hotkey string panics).

Layer: pure.
