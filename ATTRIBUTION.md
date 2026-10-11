# Attribution

Every bundled asset (icons, sounds, fonts, sample themes, test media) must be openly
licensed and listed here: path, source, author, licence. `cargo xtask audit` checks that
each file under `assets/` has an entry.

| Path | Source | Author | Licence |
|---|---|---|---|
| `apps/klip-app/src-tauri/icons/` | `app-icon.svg` drawn for Scout; the other sizes generated from it with `tauri icon` | Scout contributors | MIT OR Apache-2.0 |
| `assets/fonts/` | Inter 4.1 (`Inter-Bold.ttf`, `Inter-ExtraBold.ttf` from the release's `extras/ttf`, licence in `Inter-LICENSE.txt`), https://github.com/rsms/inter/releases/tag/v4.1 | The Inter Project Authors (Rasmus Andersson) | SIL Open Font License 1.1 |

The tray icons are drawn at runtime (`apps/klip-app/src-tauri/src/tray.rs`). The UI uses
the system fonts; the bundled Inter files are only for the text the editor draws into
clips (`klip-edit`, ADR 0017), embedded in the executable with `include_bytes!`.

## Runtime libraries

- GStreamer and its plugins: LGPL-2.1-or-later, dynamically linked. Plugins under GPL
  (for example `x264enc`, `x265enc`) are never used or bundled.
