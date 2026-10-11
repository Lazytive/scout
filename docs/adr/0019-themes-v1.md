# 0019: Themes v1: custom theme files, accent, density, text size, motion

Status: accepted (M10.5, 2026-10-11: settings, theme files, commands and the UI exist
and are checked in the mock and the real app; ROADMAP "Themes v1 (M10)"). Author:
Claude Opus 5.5.

## Context
M6.7 shipped the colour tokens (ADR 0004) and the built-in themes Dark, Graphite, Light,
OLED black and Follow system. M10 (KLIP_PROMPT §2 "Themes") adds an accent colour picker,
**custom theme files** (colours, corner roundness, font, optional background picture)
with import/export and a contrast warning, density (compact/comfortable), text size and
reduced motion; the overlay follows the theme. Theme files come from other people, so
they are hostile input (AGENTS rule 3). The UI's tokens and contrast tests live in
`apps/klip-app/src/lib/themes.ts`.

## Decisions
1. **Settings** (`appearance`, klip-settings): `theme` gains `custom`; new
   `custom_theme` (id), `accent` (`#rrggbb`, `null` = the theme's own), `density`
   (`comfortable` = the existing look, default; `compact`), `text_size` (`small` 90 %,
   `default`, `large` 115 %, `larger` 130 %), `motion` (`system` default, `reduce`,
   `full`). Repaired and strictly patched like every other setting; `custom` without a
   valid id becomes `system`.
2. **One file per theme, JSON** (`*.klip-theme`, format `"klip_theme": 1`): `name`,
   `base` (a built-in: dark, graphite, light, oled), `colors` (token → `#rrggbb`, keys =
   the CSS variable names without `--`; `hover` is derived from the scheme), `radius`
   (0–16 px), `font` (a family name: letters, digits, space, `.`, `_`, `-`, so nothing
   reaches CSS as syntax), `background` (`image` as a `data:image/…;base64,` URI, `dim`
   0–90 %, `opacity` 60–100 %). One self-contained file is easy to share; a zip would
   need a zip parser for no gain. The picture is checked by its magic bytes (PNG, JPEG,
   WebP only, ≤ 8 MiB decoded; the URI's type is ignored); the file is capped at 12 MiB
   before parsing.
3. **Lenient import, strict refusal of non-themes.** Like the settings file: wrong
   fields are dropped one at a time with an issue (shown to the user), but a file that
   isn't JSON, has no or a newer format version, or is too large is refused.
4. **Stored unpacked** in `<settings dir>/themes/<id>.json` (≤ 64 KiB) plus
   `<id>.<png|jpg|webp>`. Ids are `[a-z0-9-]`, ≤ 40, made from the name. The UI shows the
   picture through Tauri's asset protocol (the folder is added to the asset scope, not
   recursively), so no megabytes of base64 cross the IPC or the 64 KiB control-channel
   line limit. Export re-embeds the picture.
5. **Commands** (everything is a command): `theme.list`, `theme.save` (from the editor;
   the picture from a file the user picked, kept, or removed), `theme.delete` (deleting
   the theme in use switches to Follow system), `theme.import {path}`,
   `theme.export {id, path}`. Writes are atomic.
6. **Applying is the front end's job** (`src/lib/appearance.ts`): the effective tokens are
   base → custom colours → accent (the accent's strong, text, on-accent and focus
   variants are derived so they keep AA contrast where possible; a custom theme that
   sets its accent gets the same derived shades unless it sets them too); radius, font, density
   and text size are CSS variables (`--radius`, `--font`, `--space-*`, `--text-scale`);
   reduced motion is a root attribute that zeroes transitions and animations. The
   contrast warning lists the text/background pairs (`textPairs`, `uiPairs`) below WCAG
   AA for the effective tokens; with a picture, the big surfaces become translucent and
   the check blends them over the picture's darkest and lightest (dimmed) pixels. A
   warning never blocks: it's the user's theme.
7. **The background picture is the window backdrop**: the gutters show it and the large
   surfaces (gutter, chrome, status bar, panels, viewer) get the theme's opacity over it;
   raised surfaces (menus, popups) and inputs stay opaque for legibility.
8. **First paint**: the last applied variables are cached in `localStorage` and applied
   before the UI mounts, so a custom theme doesn't flash Dark at start.

## Consequences
- Fonts are referenced by name only; a missing font falls back to the default stack. No
  font files are bundled or imported (licences, ATTRIBUTION).
- Custom CSS and a theme gallery stay "Later" (KLIP_PROMPT).
- Layout-sensitive views (timeline rows, cards) must use the text and space tokens, not
  fixed px, so Larger text and Compact density don't clip; M10.5 checks every page.
