# klip-cli

Recorder CLI (M1, audio M2). `klip-cli daemon` runs the same engine as the desktop app
(M6, ADR 0014); every other command talks to whichever of them runs, through the control
channel.

```
klip-cli record [--buffer 120s] [--fps 60] [--bitrate 20000] [--max-size 1920x1080]
                [--window-title TEXT | --window-handle HWND | --monitor N]
                [--codec h264|h265|av1] [--encoder ELEMENT] [--out DIR]
                [--max-buffer-mb N] [--no-cursor]
                [--audio game|system|apps|none] [--app EXE]... [--mic [DEVICE]]
                [--exclude-app EXE]... [--no-default-excludes]
                [--audio-bitrate 160] [--no-separate-tracks]
                [--portal monitor|window|any] [--portal-token FILE]   # Linux
                [--game EXE]           # Linux: the game's program, for the Game track
                [--hotkey [KEYS]] [--hotkey-length 30s]   # Windows: RegisterHotKey; Linux: portal
                [--session] [--session-dir DIR]          # full-session recording
                [--record-hotkey [KEYS]] [--bookmark-hotkey [KEYS]]
klip-cli daemon [--settings FILE] [--poll 2s]   # the app's engine without a window
klip-cli clip [--last 30s]        # save from the running recorder; prints JSON
klip-cli session start|stop|toggle   # full-session recording in the running recorder
klip-cli bookmark                 # mark this moment (recording + clips that cover it)
klip-cli finalize FILE            # finish a recording a killed recorder left
klip-cli status | stop
klip-cli call COMMAND [JSON]       # any command, e.g. call settings.set '{"overlay":{"sound":false}}'
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

`tests/bench.rs` is the benchmark behind `cargo xtask bench` (see ROADMAP for the
method and numbers). e2e tests place the test game on a secondary monitor;
`KLIP_E2E_MONITOR=offscreen` puts it beyond all monitors instead (window capture still
works there).

`daemon` (ADR 0014) runs Klip's engine without a window: it reads the app's settings
file (or `--settings`), buffers while a detected game runs (per-game clip folders,
per-game profiles), binds the hotkeys from the settings (Windows: RegisterHotKey; Linux:
the portal), serves the control channel, and prints every engine event as a JSON line
(`ready`, `game_started`, `recorder_started`, `clip_saved`, …). `klip-cli clip`,
`status`, `session`, `bookmark` and `stop` talk to it like to `record`. Its recorder
runs in a copy of `klip-cli` (`--klip-recorder`, ADR 0016; `main` calls
`klip_engine::worker_main()`) that ends with the game; thumbnails come from the media
worker (`--klip-media-worker`, ADR 0015). e2e: `tests/m6_daemon.rs`; recorder process
input checks: `tests/workers.rs`.

`record` prints one JSON line `{"event":"ready",...}` to stdout, logs to stderr, and
serves the control channel (discovery file in `%LOCALAPPDATA%\Klip\control.json`, or
`--discovery`). Bind `klip-cli clip` to a desktop shortcut as a hotkey fallback.

Full-session recording (ADR 0013): `--session` (or `klip-cli session start`) writes
`Klip_Recording_<time>.mp4` into `--session-dir` (default: the clip folder) as a
fragmented MP4, so a crash leaves a playable file; stopping (or stopping the recorder)
finalises it into a regular MP4 and prints `{"event":"recording_stopped",...}`.
Bookmarks become chapters ("Bookmark n") in the recording and in clips that cover them.
At start, `record` finalises recordings a killed recorder left in that folder
(`{"event":"recovered",...}`).

Linux (ADR 0012): `record` captures through the desktop's screen-cast portal (the only
way on Wayland; `--window-*`/`--monitor` are Windows options). The first run shows the
desktop's dialog; the restore token in `$XDG_STATE_HOME/Klip/screencast-token` (or
`--portal-token`) makes later runs start without asking. The ready line reports the
stream (`portal`: node, kind, logical position and size). The portal doesn't say which app
is on the captured screen, so audio defaults to system audio; `--game EXE` records that
program's audio as the Game track instead (as window capture does on Windows).
`--hotkey` binds `clip.save` through the desktop's GlobalShortcuts portal (suggested keys
`ALT+F10`; the desktop confirms them once); each press saves `--hotkey-length` and prints
`{"event":"clip",...}`. Without the portal, bind `klip-cli clip` in the desktop's
shortcut settings instead.

Default clip folder: `Videos\Klip`. Default byte cap: estimated from buffer length and
bitrate (120 s at 20 Mbit/s ≈ 454 MB).

Acceptance tests (run with `cargo xtask e2e`): `tests/m1_acceptance.rs` (video) and
`tests/m2_audio.rs` (A/V sync with the beeping test game against its GPU-completed frame
log and loopback-tap beep log, per-stream stamp latency, mic track layout, excluded app
left out). `diag_system_loopback_latency` (run by name) measures the system-audio path.
Helpers are in `tests/common`. `status` shows `audio.process_lag` (measured game-audio
lag) and `clock_base_ns`; `clip` returns `start_ns` (QPC-based clip origin).
