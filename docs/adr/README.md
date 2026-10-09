# Architecture decision records

One file per decision: context, decision, consequences. Status is `accepted`, `proposed`
(to be confirmed by a later milestone) or `superseded by NNNN`.

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-workspace-and-layering.md) | Rust workspace, crate layering, lint policy | accepted |
| [0002](0002-gstreamer-capture-and-encode.md) | GStreamer (gstreamer-rs) for capture and encode; encoder allowlist | accepted |
| [0003](0003-own-mp4-writer.md) | Own pure-Rust MP4 writer for clip saves | accepted |
| [0004](0004-tauri-svelte.md) | Tauri 2 + Svelte 5 + TypeScript for the desktop app | accepted |
| [0005](0005-audio-codecs.md) | AAC-LC in clips; per-OS encoder choice | proposed (confirm in M2) |
| [0006](0006-control-channel.md) | Loopback JSON-lines control channel with token file | accepted |
| [0007](0007-test-strategy.md) | Test game, barcodes, ffmpeg as oracle, `xtask e2e` gate | accepted |
