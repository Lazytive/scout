# klip-capture

GStreamer pipelines that capture through OS APIs and encode, delivering **encoded
packets** (`klip_media::Packet`) to a `PacketSink` (the replay ring). Design and
measurements of the timing: ADR 0008.

## Video (Windows, ADR 0002, 0010)

Default (`CaptureApi::Auto` / `Klip`), built element by element:

```
klip_platform::GpuCapture (own WGC, D3D11 device on the encoder's GPU, shared via GstContext)
  → shader pass: client-area crop, scale to W×H, letterbox; HDR monitor: FP16 → tone-map
  → appsrc (D3D11Memory BGRA W×H, pts = WGC capture time on the TimeBase, + meta; leaky, 4)
  ! videorate drop-only=true ! framerate=F/1               (frame nearest each output slot)
  [probe: pts = FrameGrid.snap(capture time)]
  ! d3d11convert ! video/x-raw(memory:D3D11Memory),format=NV12,width=W,height=H
  ! [d3d11download ! cudaupload | d3d12upload | d3d11download]   (CUDA / D3D12 / software)
  ! queue leaky=downstream max-size-buffers=8              (drops counted: encoder_queue_drops)
  ! <encoder> ! <parser> ! stream-format=avc|hvc1|obu-stream ! appsink
```

Fallback (`Auto` when the own capture can't start, or `GstWgc`): the same chain fed by
`d3d11screencapturesrc capture-api=wgc` polled at 8x the capture rate with frames
re-stamped on arrival (it stamps frames with its own clock slots; ADR 0008).

- Output size is fixed at start (`fit_size`: scale down to the preset box, keep aspect,
  even dimensions), so window resizes never change encoder caps mid-buffer.
- Encoder settings per family (`configure_encoder`, ADR 0009): VBR at the target with
  1.5x peaks, one IDR per second, no B-frames; NVENC p4 high-quality + spatial AQ; MF
  `pcvbr`; D3D12 VBR; AMF/QSV from the GStreamer docs (unverified on hardware).
  Properties are set through `props::set_str`, which checks the property spec first
  (glib setters panic otherwise).
- `EncodeChain::adapter_luid`: the encoder's GPU, used to put the capture device there.
- `InputOrigin`: the first timestamp entering the encoder (pad probe). GStreamer's NVENC
  elements add 1000 hours to output timestamps (`min-pts`); `EncodeChain::attach` moves
  packets back so video shares a timeline with audio.
- `track_info_from_caps` / `track_info_with_frame`: codec, `codec_data` (avcC/hvcC, AAC
  AudioSpecificConfig; AV1's av1C is built from the first keyframe's sequence header),
  size/colorimetry or rate/channels from parser caps.
- The appsink callback runs under `catch_unwind`; errors and panics are counted in
  `CaptureStats` and reported to the sink; a bus thread reports pipeline errors and
  warnings.
- `build_encode_chain` (→ `EncodeChain`) + `CaptureSession::run` are public so tests can
  feed known frames from an `appsrc` (see `klip-recorder/tests/synthetic_encode.rs`).
- `TimeBase`: one clock (system clock = QPC on Windows) and base time for every pipeline
  of a recording; `base_clock_ns` maps clip times to other QPC-based logs.

## Video (Linux, ADR 0012)

```
klip_platform::ScreenCast (portal session; shared as PortalSession by every pipeline)
  → pipewiresrc fd=<remote> path=<node> ! video/x-raw (system memory)
  [probe: keep the compositor's frame time in the arrival meta]
  ! videorate drop-only=true ! framerate=F/1
  [probe: pts = FrameGrid.snap(capture time)]
  ! NVENC:   glupload ! glcolorconvert ! RGBA ! glcolorscale ! RGBA WxH ! glcolorconvert ! NV12 (GL)
    VA-API:  vapostproc ! NV12 WxH (VA memory)
    openh264: videoconvertscale ! I420 WxH
  ! queue leaky ! <encoder> ! <parser> ! appsink
```

- `VideoTarget::Portal` + `VideoConfig::portal`. The output size comes from the stream's
  first caps: a probe on `pipewiresrc` sizes the chain's capsfilters
  (`EncodeChain::set_size`) as they pass; `start` waits up to 5 s for them.
- Frames are stamped on arrival minus the compositor's latency (24 ms on KDE, measured;
  0 elsewhere; `KLIP_VIDEO_LATENCY_MS` overrides): KWin's own stamp is the previous
  refresh's presentation time (ADR 0012). `KLIP_CAPTURE_STAMPS=source` keeps
  `pipewiresrc`'s stamps (A/B runs).
- The desktop ending the screen-cast session (the user stopped sharing) becomes a capture
  error (`watch_portal`).
- `KLIP_PORTAL_CAPS` overrides the source caps (diagnosis; e.g. DMA-BUF, see ADR 0012).

## Audio (`audio`, `lag`, `apps`)

```
klip_platform::AudioCapture (WASAPI thread, QPC-stamped packets)
  → DriftAligner (continuous samples following the capture times) → appsrc F32 48 kHz stereo
  → audiorate ─┐
one input:     → AAC chain → appsink
several:       → tee → queue → audiomixer → AAC chain → appsink   (track "Mix")
                    └→ queue → AAC chain → appsink                (each input, optional)
AAC chain:     audioconvert → S16LE → queue → AAC encoder bitrate → aacparse → raw AAC
               (mfaacenc; Linux fdkaacenc, else avenc_aac with F32; `aac_encoder_delay`
               shifts packets back by each encoder's measured delay, ADR 0005)
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

`thumbnail::jpeg_near_end(path, back, max_width)`: decodes the keyframe at or before
`back` before the clip's end (decodebin: hardware decoders rank first) and encodes a JPEG
(`jpegenc`); used for the overlay and the library. Checked for every working encoder's
output (H.264, HEVC, AV1) in `klip-recorder`'s round-trip test: first call 0.55 s
(decoder start-up), then 15–35 ms for a 640x360 clip.

Layer: media engine (GStreamer). Linux sources (portal + PipeWire) arrive in M4.
