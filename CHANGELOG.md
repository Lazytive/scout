# Changelog

Every change, with who made it and why. Newest first. Format: `task — summary (author:
reason)`.

## Unreleased

- M2.4 — App selection: `klip_platform::{list_processes, audio_sessions}`,
  `klip_capture::apps` (default list of music players left out of system audio,
  `plan_system`: whole mix / exclude one tree / per-app capture, `app_roots` so
  multi-process apps are captured once), `AudioSource::Apps`, CLI `--audio apps`, `--app`,
  `--exclude-app`, `--no-default-excludes`, `audio-apps`. (Claude Opus 5.5: keep other
  people's music out of shared clips)
- M2.3 — A/V sync acceptance: test game `--beep`, `--beep-log`, `--tap-log` (its own
  loopback measurement of each beep), `--background`, frame log after GPU completion;
  `klip-testkit` `audio` (beeps, onsets) and `sync`; `tests/m2_audio.rs` (sync ≤ 1 frame
  with per-stream stamp latency, mic track layout, excluded app left out); e2e helpers in
  `tests/common`; `diag_*` diagnostics skipped by `cargo xtask e2e`. (Claude Opus 5.5: M2
  acceptance)
- M2.3 — Video timing: the WGC source polls at 8x the capture rate, frames are stamped on
  arrival (kept in a `ReferenceTimestampMeta`), `videorate drop-only` returns to the
  capture rate, and kept frames are snapped to a frame grid that re-centres on the
  arrival phase. The source's own stamps were up to a frame late by a per-run amount.
  (Claude Opus 5.5: found by the sync test's per-stream breakdown)
- M2.1 — Audio capture rewritten: own WASAPI reader in `klip-platform` (`AudioCapture`:
  loopback, process loopback, input; QPC capture stamps; silence when idle; retry on
  failure), `klip_media::DriftAligner`, `appsrc` inputs, live process-loopback lag
  measurement against the system mix (`klip-capture/src/lag.rs`). Replaces
  `wasapi2src`, whose arrival-time stamps put audio −20 to +23 ms off, differently per
  run and source type. (Claude Opus 5.5: sync measurements)
- M2.1 — Fix: heap corruption in process-loopback activation: the `windows` crate's `Drop`
  for `PROPVARIANT` freed the stack blob; now `ManuallyDrop`. Regression test:
  `audio_capture_is_continuous_and_qpc_stamped`. (Claude Opus 5.5)
- M2.2 — AAC tracks: `build_aac_chain` (`mfaacenc` → `aacparse`), AAC caps → `TrackInfo`,
  `tests/synthetic_audio.rs` (beeps land within 0.01 ms; no encoder-delay correction
  needed), MP4 track names / enabled flag / audio alternate group, ring snapshots keep
  track indices, ADR 0005 accepted, ADR 0008. (Claude Opus 5.5: M2)
- M2.2 — Saves cut at the request time and wait up to 250 ms for audio to cover the end
  (`Ring::snapshot_until`); audio used to stop ~60 ms before the video. (Claude Opus 5.5:
  found in the first audio clips)
- M2.1 — Recorder audio tracks and status (`audio`, `clock_base_ns`, clip `start_ns`),
  shared `TimeBase` for all pipelines, CLI `--audio`, `--mic`, `--audio-bitrate`,
  `--no-separate-tracks`, `audio-devices`. (Claude Opus 5.5: M2)
- M2.1 — Fix: NVENC output timestamps carried a 1000-hour offset (GStreamer `min-pts`);
  harmless for video-only clips, but every audio packet was evicted as "older than the
  video". The first encoder input time is now recorded and packets moved back
  (`InputOrigin`), with a regression test. (Claude Opus 5.5: found by the audio smoke test)
- M2 — Video timestamps snapped to the capture frame grid (`klip_media::FrameGrid`):
  constant frame rate where no frame is missing, all frames kept. (Claude Opus 5.5: CFR
  for editors)

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
