# Architecture decision records

One file per decision: context, decision, consequences. Status is `accepted`, `proposed`
(to be confirmed by a later milestone) or `superseded by NNNN`.

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-workspace-and-layering.md) | Rust workspace, crate layering, lint policy | accepted |
| [0002](0002-gstreamer-capture-and-encode.md) | GStreamer (gstreamer-rs) for capture and encode; encoder allowlist | accepted |
| [0003](0003-own-mp4-writer.md) | Own pure-Rust MP4 writer for clip saves | accepted |
| [0004](0004-tauri-svelte.md) | Tauri 2 + Svelte 5 + TypeScript for the desktop app | accepted |
| [0005](0005-audio-codecs.md) | AAC-LC in clips; per-OS encoder choice | accepted (M2) |
| [0006](0006-control-channel.md) | Loopback JSON-lines control channel with token file | accepted |
| [0007](0007-test-strategy.md) | Test game, barcodes, ffmpeg as oracle, `xtask e2e` gate | accepted |
| [0008](0008-audio-capture-and-sync.md) | Audio sources, track layout, one timeline for all pipelines | accepted |
| [0009](0009-encoder-selection.md) | Encoder ranking, isolated probes, cache, runtime fallback | accepted (M3) |
| [0010](0010-own-wgc-capture-and-hdr.md) | Own WGC capture on a shared D3D11 device; HDR → SDR tone-mapping | accepted (M3) |
| [0011](0011-capture-fallbacks.md) | Capture fallbacks: window → monitor → DXGI, supervisor | accepted (M3) |
| [0012](0012-linux-capture.md) | Linux: screen-cast portal + PipeWire, GL → NVENC, VA-API, openh264 | accepted (M4) |
| [0013](0013-full-session-recording.md) | Full-session recording: fragmented MP4, finalise in place, bookmarks as chapters | accepted (M5) |
| [0014](0014-app-architecture.md) | App architecture: settings, library, engine crates; thin Tauri shell; hotkeys, detection, storage, themes, overlay, bench method | accepted (M6) |
| [0015](0015-library.md) | Library: plain files, rebuildable info/thumbnail caches, sprite-strip hover-scrub, thumbnails in a child process, `klipthumb` scheme | accepted (M7) |
| [0016](0016-recorder-process.md) | Recorder in a child process of the app's executable, for the game's lifetime; overlay thumbnail via the media worker | accepted (M6.12) |
| [0017](0017-editor-normal-mode-and-export.md) | Editor Normal mode: project file, shared timeline math, preview via `asset:` + proxy, Klip-rendered text, own export pipeline in the media worker, Discord size-fit | proposed (M8) |
