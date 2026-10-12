# 0001: Rust workspace, crate layering, lint policy

Status: accepted (M0)

## Context
Klip has to run for hours next to games without crashing, and several agents may work on
it. The brief requires a Rust workspace (`crates/*`, `apps/*`, `xtask`), strict layering
(capture/encode/buffer/mux never depend on UI) and "never crash" rules.

## Decision
- Crates, bottom to top:
  - **pure** (no GStreamer, no OS APIs): `klip-media` (shared media types), `klip-buffer`
    (replay ring of encoded packets), `klip-mp4` (MP4 writer), `klip-control` (command
    protocol + loopback server/client), `klip-testkit` (barcode spec + clip analysis; test
    support only), `klip-share` (the share protocol, M11, ADR 0021).
  - **platform**: `klip-platform`, the only crate allowed `unsafe` (Win32/DXGI calls).
  - **media engine** (GStreamer): `klip-probe` (encoder discovery/ranking),
    `klip-capture` (pipelines → packets), `klip-recorder` (capture + ring + save).
  - **apps**: `klip-cli`, `klip-testgame`, later `klip-app` (Tauri), `klip-share-server`
    (the self-hostable share server, binary `scout-share`, M11.6).
- `cargo xtask layers` enforces: pure crates have no `gstreamer*`/`windows*` deps; no
  engine crate depends on an app or on `tauri*`; `unsafe` appears only in `klip-platform`.
- Lints in `[workspace.lints]`: `unsafe_code`, `unwrap_used`, `expect_used`, `panic`,
  `todo`, `unimplemented`, `unreachable`, `indexing_slicing`, `arithmetic_side_effects`,
  `undocumented_unsafe_blocks` all deny (clippy runs with `-D warnings`). Tests may use the
  panicking helpers (`clippy.toml`; integration-test files allow them at file level). The
  test game, `klip-testkit` and xtask allow `arithmetic_side_effects` at crate level since
  they're dev tools, not product code.
- Toolchain pinned in `rust-toolchain.toml`; edition 2024.

## Consequences
Checked math everywhere in product code is verbose, but overflow bugs on hostile media
can't slip in. New crates must be classified in `xtask/src/layers.rs`.
