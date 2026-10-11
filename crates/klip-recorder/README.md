# klip-recorder

Ties capture, the replay ring and the MP4 writer together.

- `RingSink`: a `klip-buffer` ring behind a mutex that implements
  `klip_capture::PacketSink` (track 0 video, then audio tracks). Leading delta frames
  before the first keyframe are dropped silently; other rejects are counted and logged;
  the last error and warning are kept for status.
- `save_from(sink, dir, length, names)`: notes the video end, waits up to
  `AUDIO_CATCH_UP` (250 ms) for live audio tracks to reach it (stalled tracks are not
  waited for), snapshots up to that end under the lock (refcount copies only), then writes
  `Scout_YYYY-MM-DD_HH-MM-SS[_n].mp4` (`Klip_…` before the rename, ADR 0020) atomically outside the lock. Tracks get their names;
  only the first audio track is enabled (ADR 0008). Measured 3–9 ms for 10–30 s video-only
  clips; with audio the wait adds the audio latency (see ROADMAP).
- `Recorder`: one `TimeBase`, the video `CaptureSession` and (if configured) an
  `AudioSession`. Audio is best effort: if it can't start, video records anyway and
  `status.audio.error` says why. Serialises saves, reports status JSON, and implements
  `klip_control::Handler` for `clip.save` (`last_ms`, 1 s–1 h), `recorder.status`,
  `recorder.stop`, `ping`.
- Start-time encoder fallback (ADR 0009): the configured encoder, then
  `encoder_fallbacks`, until one delivers its first packet; attempts are in
  `encoder_attempts()` and the status.
- `supervisor` (ADR 0011): a thread that switches video capture between the chosen
  window, its monitor (WGC) and DXGI when window capture can't see the game (exclusive
  fullscreen, stall while fullscreen, failure) and back; `decide` is the pure rule set.
  Restarts keep the encoder and the pinned output size so the buffer carries on;
  `status.video` shows the mode and switches. A window that can't be captured at start
  is recorded through its monitor.
- `session` (ADR 0013): full-session recording. `RingSink::start_session` attaches a tap
  (fed under the ring lock, starting with the ring's newest GOP) to a writer thread that
  writes a fragmented MP4 (fragments at video keyframes, max 4 s / 64 MiB, newest packet
  per track held back), a `<file>.recording.json` sidecar with the bookmarks, stops on
  low disk space (2 GiB reserve) or a full queue (512 MiB), rolls over to a new file when
  a track description changes; `RingSink::stop_session` waits for audio to reach the
  video end (≤ 250 ms), cuts there and finalises. `recover_recordings(dir)` /
  `finalize_recording(file)` finish what a killed recorder left.
- Bookmarks: `Recorder::bookmark` stores the capture time; recordings get them as
  chapters at finalise, clips get those in their range. Commands `session.start`,
  `session.stop`, `session.toggle`, `bookmark.add`; `status.session`.
- `estimate_buffer_bytes`: RAM estimate / default byte cap = bitrate x 1.5 (VBR peak) x
  (length + 1 s); the CLI passes video + all audio tracks.

Tests: unit tests for saves (unique names, atomic files, empty buffer); hardware test
`tests/synthetic_encode.rs` pushes 600 barcoded frames through the real encoder, ring and
writer and checks every frame comes back exactly once, and that cuts start on keyframes;
`tests/synthetic_session.rs` records 600 barcoded NVENC frames as a session, checks a
mid-recording copy (a kill) decodes as a continuous prefix, that finalising it keeps
those frames, and that the stopped file holds all 600 with the bookmark chapter;
`tests/synthetic_audio.rs` pushes PCM with beeps through the real AAC chain and checks
each beep lands on its input time after MP4 + ffmpeg decode (runs in `cargo test` when
mfaacenc and ffmpeg are present); `tests/encoder_fallback.rs` (a missing first encoder
falls back to the next). The supervisor's switches are tested end to end in
`apps/klip-cli/tests/m3_fallback.rs`.

Layer: media engine.
