# klip-cli

Recorder CLI (M1). The desktop app (M6) will host the same recorder and command set.

```
klip-cli record [--buffer 120s] [--fps 60] [--bitrate 20000] [--max-size 1920x1080]
                [--window-title TEXT | --window-handle HWND | --monitor N]
                [--codec h264|h265|av1] [--encoder ELEMENT] [--out DIR]
                [--max-buffer-mb N] [--no-cursor]
klip-cli clip [--last 30s]        # save from the running recorder; prints JSON
klip-cli status | stop
klip-cli probe [--codec h264]     # GPUs + every allowed encoder, tested
klip-cli monitors | windows TEXT
```

`record` prints one JSON line `{"event":"ready",...}` to stdout, logs to stderr, and
serves the control channel (discovery file in `%LOCALAPPDATA%\Klip\control.json`, or
`--discovery`). Bind `klip-cli clip` to a desktop shortcut as a hotkey fallback.

Default clip folder: `Videos\Klip`. Default byte cap: estimated from buffer length and
bitrate (120 s at 20 Mbit/s ≈ 454 MB).

Acceptance test: `tests/m1_acceptance.rs` (run with `cargo xtask e2e`).
