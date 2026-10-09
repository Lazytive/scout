# Changelog

Every change, with who made it and why. Newest first. Format: `task — summary (author:
reason)`.

## Unreleased

- M1.12 — Acceptance: `apps/klip-cli/tests/m1_acceptance.rs` (test game + record + clip,
  ffprobe/ffmpeg oracle, frame continuity) and `crates/klip-recorder/tests/synthetic_encode.rs`
  (exact frame round trip through NVENC, ring, MP4). (Claude Opus 5.5: M1 acceptance)
- M1.12 — Snapshot treats a keyframe ≤ 1 ms after the cut target as at the target, with a
  regression test; truncated 1/60 s timestamps had added a whole extra GOP. (Claude Opus
  5.5: found by the synthetic test)
- M1.11 — `klip-cli`: record, clip, status, stop, probe, monitors, windows. (Claude Opus 5.5)
- M1.10 — `klip-recorder`: ring sink, atomic clip saves, status, command handler. (Claude
  Opus 5.5)
- M1.9 — `klip-control`: JSON-lines loopback control channel with token file. (Claude Opus 5.5)
- M1.8 — `klip-capture`: WGC → d3d11convert → NV12 → encoder → parser → appsink pipeline,
  panic-free property setting. (Claude Opus 5.5)
- M1.7 — `klip-probe`: encoder allowlist/denylist, ranking, dummy-encode probe. (Claude
  Opus 5.5)
- M1.6 — `klip-platform`: DXGI adapters, windows, monitors, DPI awareness (only crate with
  `unsafe`). (Claude Opus 5.5)
- M1.5 — `klip-testgame`: D3D12 wgpu window with barcode, motion, flash; self-paced fps.
  Rendering moved out of `RedrawRequested` because WM_PAINT scheduling made it miss 120 fps.
  (Claude Opus 5.5)
- M1.4 — `klip-testkit`: barcode spec/renderer/reader, ffmpeg oracle, continuity
  analysis. (Claude Opus 5.5)
- M1.3 — `klip-mp4`: faststart MP4 writer. (Claude Opus 5.5)
- M1.2 — `klip-buffer`: replay ring. (Claude Opus 5.5)
- M1.1 — `klip-media`: shared media types. (Claude Opus 5.5)
- M0 — Repository, rules (`AGENTS.md`), ADRs 0001–0007, workspace lints, `cargo xtask`
  gates (ci, layers, audit, e2e). winit built without the Adwaita decorations feature so
  `cargo deny` stays clean (it pulled the unmaintained `ttf-parser`). (Claude Opus 5.5:
  initial setup per the build brief)
