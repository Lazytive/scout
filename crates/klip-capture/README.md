# klip-capture

GStreamer pipelines that capture through OS APIs and encode, delivering **encoded
packets** (`klip_media::Packet`) to a `PacketSink` (the replay ring). Design and
measurements of the timing: ADR 0008.

## Video (Windows, ADR 0002)

Built element by element (no launch strings):

```
d3d11screencapturesrc capture-api=wgc [window-handle + window-capture-mode=client | monitor-handle]
  ! video/x-raw(memory:D3D11Memory),framerate=8xF/1       (polls for new WGC frames, 240–1000 Hz)
  [probe: pts = arrival time on the TimeBase, + ReferenceTimestampMeta]
  ! videorate drop-only=true ! framerate=F/1               (frame nearest each output slot)
  [probe: pts = FrameGrid.snap(arrival)]
  ! d3d11convert ! video/x-raw(memory:D3D11Memory),format=NV12,width=W,height=H
  ! [d3d11download ! cudaupload]   (CUDA NVENC only; d3d11download for software encoders)
  ! queue leaky=downstream max-size-buffers=8
  ! <encoder> ! <parser> ! stream-format=avc|hvc1, alignment=au ! appsink
```

- Output size is fixed at start (`fit_size`: scale down to the preset box, keep aspect,
  even dimensions), so window resizes never change encoder caps mid-buffer.
- Encoder settings per family (`configure_encoder`): NVENC = VBR, max 1.5x, p4,
  high-quality tune, no B-frames, spatial AQ, GOP = 1 s. MF/AMF/QSV/VA have best-effort
  maps; AMF/QSV/VA names get verified on that hardware in M3. Properties are set through
  `props::set_str`, which checks the property spec first (glib setters panic otherwise).
- Why the fast polling: the source stamps the newest frame once per slot of its own
  frame-rate grid (with the slot time), so at the capture rate a frame could be up to a
  frame old by a per-run amount. Polling at 8x keeps that under ~2 ms; `videorate` brings
  the rate (and the encoder's caps) back to F.
- `InputOrigin`: the first timestamp entering the encoder (pad probe). GStreamer's NVENC
  elements add 1000 hours to output timestamps (`min-pts`); `EncodeChain::attach` moves
  packets back so video shares a timeline with audio.
- `track_info_from_caps`: codec, `codec_data` (avcC/hvcC/av1C, AAC AudioSpecificConfig),
  size/colorimetry or rate/channels from parser caps.
- The appsink callback runs under `catch_unwind`; errors and panics are counted in
  `CaptureStats` and reported to the sink; a bus thread reports pipeline errors and
  warnings.
- `build_encode_chain` (→ `EncodeChain`) + `CaptureSession::run` are public so tests can
  feed known frames from an `appsrc` (see `klip-recorder/tests/synthetic_encode.rs`).
- `TimeBase`: one clock (system clock = QPC on Windows) and base time for every pipeline
  of a recording; `base_clock_ns` maps clip times to other QPC-based logs.

## Audio (`audio`, `lag`, `apps`)

```
klip_platform::AudioCapture (WASAPI thread, QPC-stamped packets)
  → DriftAligner (continuous samples following the capture times) → appsrc F32 48 kHz stereo
  → audiorate ─┐
one input:     → AAC chain → appsink
several:       → tee → queue → audiomixer → AAC chain → appsink   (track "Mix")
                    └→ queue → AAC chain → appsink                (each input, optional)
AAC chain:     audioconvert → S16LE → queue → mfaacenc bitrate → aacparse → raw AAC
```

- `AudioSource`: `System` (output loopback), `SystemExcept` / `Process` (process loopback,
  exclude/include a tree), `Apps` (several process trees mixed in one input), `Device`
  (microphone). `AudioSession::start(cfg, first_track, sink, timebase)`;
  `AudioConfig::track_labels` gives the track order.
- `lag`: process loopback stamps its packets 13–19 ms late (per session). While a
  process is recorded, the system mix is captured too (in memory only) and
  `LagEstimator` measures the lag every 2 s from the loudness envelopes; status shows it
  (`audio.process_lag`).
- `apps`: default list of music players left out of system audio, `app_roots` (one tree
  per app) and `plan_system` (whole mix / except one tree / per-app capture).
- `list_devices` (GStreamer device monitor; ids are Windows endpoint ids) and
  `process_loopback_supported`.
- A device that fails or disappears records silence and posts a warning (counted in
  `CaptureStats::warnings`, reported to the sink) and is retried.

## Measured (Windows 11, RTX 4080 SUPER, GStreamer 1.28.6; `cargo xtask e2e` 2026-10-09)

- A/V sync with the beeping test game, 60 fps capture: audio stamped 0.2–1.0 ms after the
  audio engine played each beep, video 5–10 ms after the GPU finished each frame; net
  −4.7 to −9.1 ms (audio slightly early), spread within a run < 2 ms.
- Video: 59.98 fps, frame intervals 16.67 ms (p1–p99), every captured frame new.
- `mfaacenc`: output aligned with its input (beeps within 0.01 ms), 220x real time.
- A static desktop yields about 1 fps (WGC only delivers changed frames): VFR when idle.
- Recorder CPU (game audio + timing reference + NVENC 720p60): ~6 % of one core.
- Diagnostics (`diag_*` tests, run by name): `tests/timestamp_lateness.rs` shows how
  `wasapi2src` and the WGC source stamp buffers relative to arrival.

Layer: media engine (GStreamer). Linux sources (portal + PipeWire) arrive in M4.
