# klip-testgame

A wgpu (Direct3D 12 on Windows) window used to verify clips:

- the `klip-testkit` barcode (24-bit frame counter + CRC-8) on every frame,
- a moving bar and a scrolling checker for encoder motion,
- a white flash (below the barcode band) every `fps` frames, i.e. once per second;
  the matching beep arrives with audio in M2.

```
klip-testgame [--width 1280] [--height 720] [--fps 120]
              [--mode windowed|borderless|exclusive] [--duration 60s]
              [--frames-log frames.csv] [--title "Klip Test Game"]
```

Frame pacing is done by the game (vsync off: Immediate/Mailbox), sleeping then spinning
to each deadline. Measured: 120.0 fps, frame interval p99 8.7 ms at 1280x720. After the
first frame it prints `{"event":"ready","pid":…,"hwnd":…,"width":…,"height":…,"fps":…,"gfx":…}`.

HDR output is not implemented yet (M3). Dev tool: `arithmetic_side_effects` allowed.
