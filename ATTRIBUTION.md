# Attribution

Every bundled asset (icons, sounds, fonts, sample themes, test media) must be openly
licensed and listed here: path, source, author, licence. `cargo xtask audit` checks that
each file under `assets/` has an entry.

| Path | Source | Author | Licence |
|---|---|---|---|
| `apps/klip-app/src-tauri/icons/` | `app-icon.svg` drawn for Klip; the other sizes generated from it with `tauri icon` | Klip contributors | MIT OR Apache-2.0 |

The tray icons are drawn at runtime (`apps/klip-app/src-tauri/src/tray.rs`); the UI uses
the system fonts (no bundled font files).

## Runtime libraries

- GStreamer and its plugins: LGPL-2.1-or-later, dynamically linked. Plugins under GPL
  (for example `x264enc`, `x265enc`) are never used or bundled.
