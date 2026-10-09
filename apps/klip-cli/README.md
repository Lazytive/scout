# klip-cli

Recorder CLI (M1, audio M2). The desktop app (M6) will host the same recorder and
command set.

```
klip-cli record [--buffer 120s] [--fps 60] [--bitrate 20000] [--max-size 1920x1080]
                [--window-title TEXT | --window-handle HWND | --monitor N]
                [--codec h264|h265|av1] [--encoder ELEMENT] [--out DIR]
                [--max-buffer-mb N] [--no-cursor]
                [--audio game|system|apps|none] [--app EXE]... [--mic [DEVICE]]
                [--exclude-app EXE]... [--no-default-excludes]
                [--audio-bitrate 160] [--no-separate-tracks]
klip-cli clip [--last 30s]        # save from the running recorder; prints JSON
klip-cli status | stop
klip-cli probe [--codec h264]     # GPUs + every allowed encoder, tested
klip-cli monitors | windows TEXT
klip-cli audio-devices            # inputs (for --mic) and outputs, with endpoint ids
klip-cli audio-apps               # apps with audio; which are left out by default
```

Audio (ADR 0008): recording a window records **only that game's audio** (per-process
loopback); recording a monitor records system audio **without music players**. `--app`
adds an app such as `Discord.exe` on its own track; `--mic` adds the default microphone.
With more than one input the clip's first audio track is the mix (what players play),
followed by each input (for editors) unless `--no-separate-tracks`.

`record` prints one JSON line `{"event":"ready",...}` to stdout, logs to stderr, and
serves the control channel (discovery file in `%LOCALAPPDATA%\Klip\control.json`, or
`--discovery`). Bind `klip-cli clip` to a desktop shortcut as a hotkey fallback.

Default clip folder: `Videos\Klip`. Default byte cap: estimated from buffer length and
bitrate (120 s at 20 Mbit/s ≈ 454 MB).

Acceptance tests (run with `cargo xtask e2e`): `tests/m1_acceptance.rs` (video) and
`tests/m2_audio.rs` (A/V sync with the beeping test game against its GPU-completed frame
log and loopback-tap beep log, per-stream stamp latency, mic track layout, excluded app
left out). `diag_system_loopback_latency` (run by name) measures the system-audio path.
Helpers are in `tests/common`. `status` shows `audio.process_lag` (measured game-audio
lag) and `clock_base_ns`; `clip` returns `start_ns` (QPC-based clip origin).
