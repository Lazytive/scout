# klip-capture

GStreamer pipelines that capture through OS APIs and hardware-encode, delivering
**encoded packets** (`klip_media::Packet`) to a `PacketSink` (the replay ring).

Windows video path (ADR 0002), built element by element (no launch strings):

```
d3d11screencapturesrc capture-api=wgc [window-handle + window-capture-mode=client | monitor-handle]
  ! video/x-raw(memory:D3D11Memory),framerate=F/1
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
- `track_info_from_caps`: codec, `codec_data` (avcC/hvcC/av1C), size and colorimetry
  (ISO code points) from parser caps.
- The appsink callback runs under `catch_unwind`; errors and panics are counted in
  `CaptureStats` and reported to the sink; a bus thread reports pipeline errors.
- `build_encode_chain` + `attach_sink` + `CaptureSession::run` are public so tests can
  feed known frames from an `appsrc` (see `klip-recorder/tests/synthetic_encode.rs`).

## Measured behaviour of the WGC source (GStreamer 1.28.6, Windows 11, RTX 4080 SUPER)

- A static desktop yields about 1 fps: WGC only delivers frames when content changes, so
  output is variable frame rate when the source is idle.
- Test game at 120 fps, capture at 60 fps (1280x720): 59.9–60.0 fps delivered, every
  frame new (no repeats), no gap longer than one capture tick. Frame intervals were exactly
  16.67 ms in one run and 14.6–18.6 ms (p1–p99) in another: timestamps follow frame
  arrival, so they jitter by a couple of ms.
- When the game renders below the capture rate (a 96 fps run), the source sometimes
  repeats a frame and leaves gaps (VFR). Snapping timestamps to the frame grid / a CFR
  option is planned with the A/V-sync work in M2.

Layer: media engine (GStreamer). Linux sources (portal + PipeWire) arrive in M4.
