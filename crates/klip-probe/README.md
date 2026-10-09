# klip-probe

Encoder discovery and licence/patent policy.

- `policy`: the **allowlist** (NVENC D3D11/CUDA, AMF, QSV, Media Foundation incl.
  per-device `mf<codec>device<N>enc` names, VA-API, openh264 on Linux only) and the
  **denylist** (`x264enc`, `x265enc`, GPL/software H.264/HEVC `avenc_*`, …) plus GPL plugin
  names for the bundle audit. This file is the only product source allowed to mention
  denied names (`cargo xtask audit`).
- `candidates(codec, gpus)`: allowlisted encoders GStreamer has registered, ranked: vendor
  encoders for a GPU that's present (D3D11 NVENC before CUDA NVENC), then hardware MFTs,
  then software MFT/openh264.
- `probe(choice)`: encodes 1 s of 1280x720@60 test video to check the encoder really works
  (drivers can be missing even when the plugin registers). `best()` returns the first
  working one; `probe_all()` reports all.

M3 adds: persisting the ranked fallback list, hybrid laptops (encode on the GPU that owns
the captured frames), AMD/Intel property maps verified on that hardware.

Layer: media engine (GStreamer).
