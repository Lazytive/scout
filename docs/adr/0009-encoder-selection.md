# 0009: encoder selection, probing and fallback

Status: accepted (M3, 2026-10-09).

## Context
Klip must record on NVIDIA, AMD and Intel GPUs, on hybrid laptops (iGPU + dGPU) and on
machines where a vendor runtime is missing or broken, without anyone picking an encoder
by hand. Patents limit us to GPU vendor encoders and OS encoders (AGENTS.md rule 6). An
encoder that is registered can still fail when a session opens (driver missing, session
limit, driver bug), and opening a session goes through the GPU driver, which can crash or
hang the process.

Observed on the dev machine (session 3; RTX 4080 SUPER, driver 32.0.16.1742, GStreamer
1.28.6): inside one process, opening an NVENC session (`nvd3d11h264enc`, `nvh264enc`)
failed with `NV_ENC_ERR_INVALID_VERSION` after certain other elements had been created
and dropped, and which ones depended on the build (fakesink did it in one build and not
the next). The same pipeline always worked in a fresh process. Diagnostic:
`crates/klip-probe/tests/diag_nvenc.rs`. Also: DXGI lists the GPU twice, with two
different LUIDs, because two virtual display drivers (Parsec, Virtual Desktop) are
installed; GStreamer registers per-device encoders for both.

## Decision
- **Families.** NVENC (D3D11, CUDA), AMF, QSV, Media Foundation (hardware MFTs and
  Microsoft's software MFT), D3D12 Video Encode (`d3d12h264enc`, vendor-neutral through
  the OS video API; no vendor runtime), VA-API and openh264 on Linux. Per-device element
  names (`<base>device<N>enc`) are allowed when the base name is.
- **Ranking** (`klip_probe::candidates`), lower first: encoders on the GPU that drives the
  captured monitor, then unknown-GPU ones (CUDA), then other GPUs; within each, vendor SDK
  encoders (NVENC D3D11, AMF, QSV), hardware MFT, D3D12, CUDA NVENC (needs a CPU round
  trip in our chain); base element before `deviceN`; software MFT always last. The GPU of
  an encoder comes from its family or its long name (MF/D3D12 names carry the vendor),
  **not** from its `adapter-luid` property, because reading that needs an element
  instance (see context). Limitation: two GPUs of the same vendor can't be told apart;
  the base element (first registered adapter) wins.
- **Probing** is a 1 s dummy encode in a **child process**: the app binary calls
  `klip_probe::worker_main()` first thing; `Prober::Isolated` runs
  `<exe> --klip-encoder-probe <element> <timeout_ms>` and reads one JSON line (size-capped,
  strict parse, killed after timeout + 10 s). Tests and tools may probe in-process.
- **Cache.** Results are kept in `%LOCALAPPDATA%\Klip\encoders.json` (CLI; the app will
  use the same), valid for a fingerprint of GStreamer version, PCI ids + driver version
  of each adapter and the installed encoder names; any change re-probes. Atomic writes,
  256 kB cap, invalid files ignored. `ranked()` probes in order only until the first
  working encoder; the rest stay as untested fallbacks.
- **Runtime fallback.** `Recorder::start` tries the ranked list: an encoder that fails to
  build, to start, or reports an error before its first packet (3 s) is stopped and the
  next is tried; target/platform errors end the search. Outcomes go back into the cache:
  a probed encoder that fails to start twice in a row is left out until it works again
  or the fingerprint changes; an untested one is left out at once.
- **Settings per family** (`klip_capture::configure_encoder`): VBR at the target with
  1.5x peak, keyframe every `keyframe_interval` frames, no B-frames, IDR keyframes.
  NVENC, MF and D3D12 verified on this machine by `every_working_encoder_round_trips_
  exactly` (exact frame counters back, keyframe cuts); AMF and QSV names from the
  GStreamer 1.28 documentation, unverified on hardware.

## Consequences
- First start on a new setup costs one isolated probe (~1–1.7 s here) per encoder tried;
  later starts cost nothing.
- A broken NVENC still records (with MF or D3D12, which use the same hardware here).
- AMF/QSV need a test on real AMD/Intel hardware (human checklist in ROADMAP.md).
- If the in-process NVENC failure ever hits the recorder's own pipeline, the fallback
  takes over; the cause (driver or GStreamer) is still open.
