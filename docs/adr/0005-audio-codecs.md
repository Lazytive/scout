# 0005: audio codecs

Status: accepted (M2, 2026-10-09). Proposed in M0, confirmed by the measurements below.

## Context
Clips must play everywhere people share them (Discord, phones, browsers) and import into
editors (Premiere, Resolve, CapCut). Rule 6 asks us to record codec choices.

## Decision
- Clips: **AAC-LC**, 48 kHz, stereo, 160 kbit/s per track, in MP4. Best compatibility with
  editors and phones. We use OS/system encoders rather than bundling our own:
  - Windows: Media Foundation `mfaacenc` (verified present in GStreamer 1.28.6).
  - Linux: `fdkaacenc` from the Flatpak runtime if present, otherwise `avenc_aac` (FFmpeg's
    native encoder, LGPL build only). Verify the licence of the exact build in M4.
- **Opus** stays an option (royalty-free, BSD `opusenc`) for Custom quality and future
  formats, written as `Opus` + `dOps` in MP4.
- Multiple audio tracks (game, voice, mic) are separate MP4 tracks; the first is marked
  default. Track layout (a mix first, then the separate inputs) is in ADR 0008.

## Consequences
M2 has to measure encode latency/CPU of `mfaacenc` and check A/V sync. If an editor rejects
multi-track AAC, add an export option that mixes down to one track.

## Measured (M2)

Windows 11 26200, i7-14700KF, GStreamer 1.28.6 `mfaacenc`, 48 kHz stereo, 160 kbit/s:

- Timing: PCM with beeps at known times → `mfaacenc` → `aacparse` → MP4 → ffmpeg decode puts
  every beep within 0.01 ms of its input timestamp
  (`crates/klip-recorder/tests/synthetic_audio.rs`). Media Foundation already aligns its
  output, so no encoder-delay correction is needed (`AAC_ENCODER_DELAY = 0`).
- Speed: 6 s of audio encoded in 27 ms (220x real time, including feeding the pipeline),
  i.e. well under 1 % of one core per track.
- Output: AAC-LC, `AudioSpecificConfig` `11 90`, 1024-sample frames back to back
  (21.333 ms), 158–161 kbit/s measured by ffprobe.
- Players and editors: ffprobe/ffmpeg read multi-track clips with track names and default
  flags. Premiere/Resolve/CapCut import is a manual check for the human (ROADMAP).
