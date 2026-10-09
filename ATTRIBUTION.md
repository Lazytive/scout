# Attribution

Every bundled asset (icons, sounds, fonts, sample themes, test media) must be openly
licensed and listed here: path, source, author, licence. `cargo xtask audit` checks that
each file under `assets/` has an entry.

| Path | Source | Author | Licence |
|---|---|---|---|

(No bundled assets yet.)

## Runtime libraries

- GStreamer and its plugins: LGPL-2.1-or-later, dynamically linked. Plugins under GPL
  (for example `x264enc`, `x265enc`) are never used or bundled.
