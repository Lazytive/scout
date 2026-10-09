# 0008: audio capture, track layout and A/V sync

Status: accepted (M2, 2026-10-09).

## Context
Clips need the game's sound, optionally a microphone and voice chat, in sync with the
video, without recording other people's music by accident. Capture must stay inside OS
APIs (AGENTS.md rule 1), and an audio problem must never cost the video (rule 3).

## Decision

### Sources (Windows)
Klip reads WASAPI itself (`klip_platform::AudioCapture`, a thread per input) and feeds an
`appsrc`. Every WASAPI packet carries the QueryPerformanceCounter time of its first frame;
QPC is also the clock behind WGC and GStreamer's system clock, so audio can be placed on
the video's timeline exactly. Output is float 48 kHz stereo (WASAPI converts).
- *Game audio* (default when recording a window): process loopback of the window's
  process tree. Only the game is recorded, so music players, browsers and notifications
  never are. Falls back to system audio before Windows 10 2004.
- *System audio* (default when recording a monitor): the output mix minus music players.
  A stream can leave out only one process tree, so `klip_capture::apps` plans it: no
  music app running → whole mix; one (or only one playing) → exclude that tree; two or
  more playing → capture every other app with an audio session one by one and mix them
  (apps that start later are missed until the next recording, logged). The default list
  (`DEFAULT_EXCLUDED_APPS`) holds common music/media players; users can add or clear it.
- *Apps* (`--app`): process loopback of the top-most processes with that executable name
  (one tree per app, so multi-process apps aren't recorded twice).
- *Microphone*: off by default; the default input or a chosen device.
- A device that can't be opened, fails or disappears records silence and posts a warning,
  and is retried every 2 s. While a stream delivers nothing (system loopback with nothing
  playing), silence is generated on the QPC timeline so the stream stays continuous.

GStreamer's `wasapi2src` was used first and dropped: it stamps buffers when they're read,
not when they were captured, which left audio between −20 ms (system loopback) and +23 ms
(process loopback) off, differing per run and per source type
(`klip-capture/tests/timestamp_lateness.rs`, `m2_audio` e2e).

### Timing corrections
- **Process loopback lag.** Process-loopback packets are stamped a constant 13–19 ms later
  than system loopback stamps the same sound (different per session;
  `klip-platform/tests/loopback_offset.rs`). While a process is recorded, the system mix
  is also captured as a timing reference (in memory only, a few seconds, never written)
  and the lag is measured every 2 s by cross-correlating the loudness envelopes of the two
  streams (median of the last 5 clear matches; 16 ms until the first). Envelopes, because
  raw correlation of a tonal sound is ambiguous by one period.
- **Continuity and drift.** `klip_media::DriftAligner` turns packets into back-to-back
  samples whose timeline follows the capture times: it repeats or drops single frames
  when the smoothed difference passes half a frame (device clock drift, typically under
  100 ppm), and restarts on real gaps (> 20 ms).
- **Video.** `d3d11screencapturesrc` hands over the newest WGC frame once per slot of its
  frame-rate grid and stamps it with the slot time, so at the capture rate a frame could
  be up to a frame old, differently per run. The source now polls at 8x the capture rate
  (240–1000 Hz); each frame is stamped with its arrival time on the shared clock (also kept
  in a `ReferenceTimestampMeta`), `videorate drop-only` keeps the frame nearest each output
  slot and sets the encoder's frame rate, and the kept frame gets its arrival time back,
  snapped to the capture grid (`klip_media::FrameGrid`, which re-centres on the arrival
  phase every 30 frames).
- GStreamer's NVENC elements shift output timestamps by 1000 hours (`min-pts`); a pad
  probe records the first encoder input time and packets are moved back (`InputOrigin`).
- All pipelines of a recording share one `TimeBase` (system clock + base time). Clip saves
  cut at the video end at request time and wait (≤ 250 ms) for audio to cover it.

### Track layout
One input → one track. Several inputs → track 1 is the **mix** (browsers, Discord and
phones only play the first audio track), then each input on its own track for editors
(can be turned off). Tracks are named (`hdlr`), all audio tracks share alternate group 1
and only the mix is enabled.

## Consequences
- Measured with the test game (`apps/klip-cli/tests/m2_audio.rs`; references: the game's
  GPU-completed frame log and its own loopback tap of each beep): audio is stamped 0.2–1.0
  ms after the audio engine played it, video 5–10 ms after the GPU finished the frame (the
  compositor's next refresh plus WGC delivery). Net A/V −4.7 to −9.1 ms (audio slightly
  early), spread within one run under 2 ms; target was one frame (16.7 ms).
- Recording game audio also captures the system mix for timing. It is analysed in memory
  and discarded; documented in the privacy notes.
- Two music players playing at once makes system audio miss apps that start later; game
  audio (the default for games) has no such limit.
- Recorder CPU with game audio + timing reference + 60 fps NVENC: ~6 % of one core.
- Linux (M4): `klip_platform::AudioCapture` over PipeWire (`pipewire_audio.rs`, `pipewire`
  crate): output = the sink's monitor (`stream.capture.sink`), mic = a capture stream,
  per-app = Klip links the apps' playback streams (`Stream/Output/Audio` nodes whose
  client's `pipewire.sec.pid` is in the process tree, or every other one for "except")
  to its own unconnected capture stream; PipeWire mixes the links. Packets are stamped
  with the graph cycle's time minus the stream delay (`pw_stream_get_time_n`,
  CLOCK_MONOTONIC = GStreamer's system clock), with the same silence/overlap timeline
  as WASAPI (`timeline.rs`). An app's stream reaches Klip one graph cycle before the
  output's mix (−10 ms measured at a 512-frame quantum), so the lag estimator runs on
  Linux too, with a 0 default and a search range down to −50 ms.
