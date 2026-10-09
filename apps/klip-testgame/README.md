# klip-testgame

A wgpu (Direct3D 12 on Windows, Vulkan on Linux) window used to verify clips:

- the `klip-testkit` barcode (24-bit frame counter + CRC-8) on every frame,
- a moving bar and a scrolling checker for encoder motion,
- a white flash (below the barcode band) every `fps` frames, i.e. once per second,
- with `--beep`, a 60 ms 1 kHz tone at every flash (cpal, default output device). Each
  beep is placed using cpal's predicted playback time; `--beep-log` writes
  `index,ns_since_start` per beep, `--frames-log` writes `counter,ns_since_start` per
  frame, logged after the GPU finished the frame (`device.poll(wait)`), so GPU load
  doesn't shift it. `--tap-log` also captures the default output's loopback and logs when
  each beep actually passed the audio engine (WASAPI capture timestamps, onset found by
  correlation with the beep). All logs are flushed once a second. Together they are the
  ground truth for the A/V sync test (`klip_testkit::sync`); the ready line's `anchor`
  maps game time to QueryPerformanceCounter time.

```
klip-testgame [--width 1280] [--height 720] [--fps 120]
              [--mode windowed|borderless|exclusive] [--duration 60s]
              [--frames-log frames.csv] [--beep] [--beep-log beeps.csv] [--tap-log tap.csv]
              [--background] [--title "Klip Test Game"]
```

`--background` opens the window without taking focus (automated tests). `--monitor NAME`
(or `secondary`: a monitor not at the desktop origin) makes it borderless fullscreen on
that monitor; on Wayland, where apps can't place windows, that is how tests keep it off
the main screen. The ready line lists `monitors` (name, position, size, scale) and
`monitor_name`. On Linux the `--tap-log` reference comes from Klip's PipeWire capture of
the default sink's monitor (ALSA through cpal can't capture an output, and cpal's ALSA
clock starts at the stream), and the ready line's `anchor` is CLOCK_MONOTONIC.

Frame pacing is done by the game (vsync off: Immediate/Mailbox), sleeping then spinning
to each deadline. Measured: 120.0 fps, frame interval p99 8.7 ms at 1280x720. After the
first frame it prints `{"event":"ready","pid":…,"hwnd":…,"width":…,"height":…,"fps":…,"gfx":…,"beep_rate":…,"anchor":…}`.
Measured with the logs: cpal's predicted playback time matches the loopback tap within
1.1 ms.

HDR output is not implemented yet (M3). Dev tool: `arithmetic_side_effects` allowed.

Benchmark scene (`cargo xtask bench`): `--uncapped` renders as fast as the GPU allows
(`--fps` then only sets the animation rate) and `--gpu-load N` adds a per-pixel shader
loop of N steps (its result feeds a branch that never fires, so it can't be optimised
away and the picture is unchanged); the bench calibrates N for ~240 fps at 1920x1080.
