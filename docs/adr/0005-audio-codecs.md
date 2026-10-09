# 0005: audio codecs

Status: proposed (M0); confirm with measurements in M2.

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
  default.

## Consequences
M2 has to measure encode latency/CPU of `mfaacenc` and check A/V sync. If an editor rejects
multi-track AAC, add an export option that mixes down to one track.
