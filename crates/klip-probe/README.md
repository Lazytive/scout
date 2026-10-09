# klip-probe

Encoder discovery, licence/patent policy, probing and the probe cache (ADR 0009).

- `policy`: the **allowlist** (NVENC D3D11/CUDA, AMF, QSV, Media Foundation, D3D12 Video
  Encode, VA-API incl. low-power `va*lpenc`, openh264 on Linux only; per-device
  `<base>device<N>enc` names of each, VA's `varenderD<N><codec>enc`)
  and the **denylist** (`x264enc`, `x265enc`, GPL/software H.264/HEVC `avenc_*`, …) plus
  GPL plugin names for the bundle audit. This file is the only product source allowed to
  mention denied names (`cargo xtask audit`).
- `candidates(codec, gpus, prefer)`: allowlisted encoders GStreamer has registered,
  ranked: encoders on the GPU vendor that drives the captured monitor first (`prefer`;
  hybrid laptops), then vendor SDK encoders (NVENC D3D11 / AMF / QSV), hardware MFTs,
  D3D12, CUDA NVENC, base element before `deviceN`, software MFT last. Never creates
  element instances (see ADR 0009 for why).
- `probe(choice)`: encodes 1 s of 1280x720@60 test video to check the encoder really works
  (drivers can be missing even when the plugin registers).
- `isolated`: `Prober::Isolated(exe)` runs each probe in a child copy of the app
  (`<exe> --klip-encoder-probe <element> <ms>`); the app's `main` must call
  `worker_main()` first. `Prober::InProcess` is for tests and tools.
- `cache`: `ProbeCache` (JSON, atomic write, size-capped, hostile-input safe), valid for a
  `fingerprint` of GStreamer version + adapters (PCI ids, driver version) + installed
  encoders. Start outcomes are recorded too (`record_started`, `record_start_failure`).
- `ranked(codec, gpus, prefer, cache, prober)`: the list to try, first one known to work;
  `best()` and `probe_all()` for tools and tests.

Diagnostic: `tests/diag_nvenc.rs` re-checks the in-process NVENC failure (ADR 0009).

Layer: media engine (GStreamer).
