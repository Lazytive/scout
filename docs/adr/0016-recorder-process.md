# 0016: The recorder runs in a child process for the game's lifetime

Status: accepted (M6.12, 2026-10-09). Author: Claude Opus 5.5.

## Context
`cargo xtask bench` (M6.10) measured the engine (`klip-cli daemon`, the same engine the
app runs) at 89 MB private memory before any game and **379 MB private (285 MB
resident) after a game had been recorded and had exited**, although the `Recorder` was
stopped and dropped. What stays is not Klip's own data (the replay ring is freed) but
state the media stack keeps per process once it has been used: GStreamer elements'
plugin-level caches, the D3D11 devices and NVENC / Media Foundation sessions'
driver-side allocations, and the hardware decoder that made the overlay's thumbnail.
Most of it is outside Klip's control (driver and plugin internals), and GStreamer can't
be de-initialised and initialised again within one process. A tray app that sits next
to every game for days should not keep a game's worth of encoder memory.

A second reason: a GPU driver that crashes or hangs inside an encoder takes the whole
process down. Encoder probes already run in a fresh copy of the executable for that
reason (ADR 0009, `klip_probe::isolated`).

## Options
1. **Find and free what stays, in process.** Not possible for driver and plugin state;
   no `gst_deinit` + re-init.
2. **One recorder process per recording (chosen).** A copy of the app's own executable,
   started when a game is found and ending when it exits; the OS gives everything back.
3. **One long-lived media process** for the engine's lifetime: crash isolation, but the
   memory would stay in that process just the same.
4. **A separate `klip-recorder` binary**: a second executable to build, sign and ship,
   for no gain over a flag on the existing one (the probe workers' pattern).

## Decision
- **Who does what.** The engine keeps everything that isn't media: settings, game
  detection, hotkeys, events, the control channel, storage, the overlay's state, and
  the encoder choice (it owns the probe cache; probes stay isolated as before). The
  recorder process does what `Recorder` did in the engine: capture (with the capture
  supervisor), encode, audio, the replay ring, clip saves and full-session recordings.
  `config::plan` is split into `encoders_for` (engine) and `plan_with` (recorder
  process: capture target, audio plan, Linux screen-cast portal).
- **One executable, one more role.** The host binary (`klip-cli`, `klip-app`) calls
  `klip_engine::worker_main()` early in `main` (after `klip_probe::worker_main`, the
  media worker's `worker_main` and logging); `<exe> --klip-recorder` becomes a recorder
  process. `EngineOptions::isolation` defaults to `Isolation::auto()`: a child process
  when the executable called `worker_main`, otherwise in process (unit tests, tools);
  the engine sees one `RecorderHandle` either way.
- **Protocol** (the recorder's stdin/stdout, JSON lines; no port, no token: the pipes
  belong to the two processes):
  - engine → recorder, first line: `{"settings": …, "game": …|null, "encoders": [names]}`
    (the engine's settings, the followed game, its encoder choice best first);
  - recorder → engine, first line: `{"start": {"ok": true, "status": …, "clip_dir": …,
    "fps": …, "buffer_ms": …, "attempts": […]}}` or `{"start": {"ok": false, "error": …}}`;
    encoder attempts go into the engine's probe cache as before;
  - then `klip_control::Request` / `Response` lines (ADR 0006's format) for
    `clip.save`, `session.*`, `bookmark.add`, `recorder.status`, and two worker
    commands: `worker.status` (status + whether a recording runs, one round trip for
    `engine.status`) and `worker.finish` (stop capturing, finalise a running recording,
    answer its summary). Each request runs on its own thread in the recorder (a status
    read never waits for a save; at most 16 at once, more get `busy`).
  - The engine stops a recorder with `worker.finish`, closes the pipe and waits for the
    process to exit (30 s, then it is killed), so `recorder_stopped` means the memory is
    back. End of input without `worker.finish` (the engine was killed) makes the
    recorder finalise a running recording and exit by itself.
- **Hostile input both ways** (rule 3): lines are size-capped (2 MiB start request,
  64 KiB requests, 1 MiB replies), parsed strictly (`deny_unknown_fields`), anything
  else on the recorder's stdout is ignored; the settings go through the lenient loader;
  encoder names are resolved against the installed, allowlisted candidates (a GPL or
  unknown name is dropped, not loaded); error codes are mapped to the known set and
  messages capped. Every call has a timeout (status 5 s, saves and stopping a recording
  10 min, other commands 30 s, start 3 min since Linux may show the screen-share
  dialog); a recorder that doesn't answer is reported, never waited for forever.
- **A recorder process that dies** (driver crash, killed) is noticed when its pipe
  closes: `recorder_stopped` with the reason ("the recorder process ended unexpectedly
  (exit code …)"), a new recorder at once the first time per game (later after the
  usual 30 s retry), and the recording it left is finalised by the M5 recovery 16 s
  later (recovery leaves files written in the last 15 s alone, as another recorder could
  still be writing them).
- **The overlay's thumbnail** (which is also the clip's library poster) is decoded by
  M7's media worker (ADR 0015, `--klip-media-worker`), started for that one job and
  stopped right after (30 s timeout, JPEG read back capped at 8 MiB); `media_worker:
  None` (tests) decodes it in process as before. M7 itself made library posters and
  strips in that worker but kept this thumbnail in the engine, where its decoder stayed
  loaded. (A one-shot `--klip-thumbnail` worker was written first and dropped when M7's
  worker landed.) The recorder process stays separate from the media worker: its
  lifetime is the game's, and a driver crash in it must not take library jobs along.
- **Decoding a few key frames in a fresh process** (found while timing the poster in a
  one-shot worker, 1.27–1.36 s on a 30 s 1080p60 clip, against 1.18 s for the first and
  33 ms for later posters in the engine, whose warm decoder was the memory that never
  went away):
  - `klip_capture::thumbnail` called `klip_probe::init()`, which also starts Media
    Foundation on purpose (the NVENC/MF ordering workaround of ADR 0009): 0.72–0.77 s
    per process, needed only where encoders run. Thumbnails now only initialise
    GStreamer (33–36 ms); the engine and the recorder still call `klip_probe::init()`.
  - The media worker ranks hardware video decoders (class
    `Codec/Decoder/Video/Hardware`) down to MARGINAL at start
    (`thumbnail::prefer_software_decoders`), so `decodebin` takes a software decoder
    where one is installed (`avdec_h264`/`avdec_h265`, LGPL gst-libav; `dav1ddec`, BSD;
    `openh264dec`) and still falls back to the GPU where none is. For a few key frames
    starting a GPU decoder costs more than decoding (1080p poster in a fresh process:
    71 ms in software, 336 ms with D3D11), and it leaves the GPU's decoder to the game.
    This revises "hardware decoders first" (M6.9, ADR 0015) for the media worker only.
  - Result: a poster in a one-shot worker 132–271 ms, a 16-tile strip 437–443 ms
    (was 1.45–1.59 s; M7's e2e: strip of a visible card 248 ms, was 1.5–1.6 s).
- **Logs**: the recorder logs to stderr; the engine copies it to its own. Windows:
  it is started with `CREATE_NO_WINDOW` (no console window from the GUI app).
  Linux: the recorder process registers the portal app id itself before any portal
  call (the portals remember screen-cast choices per app id).

## Consequences
- After a game the engine is back at its idle size; measured in ROADMAP ("What Klip
  costs a game") and checked by `m6_daemon` e2e (private memory after the game within
  64 MB of before; no worker process left).
- A driver crash in capture or encode no longer takes the app, its hotkeys or its tray
  down; the buffer restarts and the recording is kept.
- Each game start pays for a process start and GStreamer initialisation in the
  recorder, and each overlay thumbnail for a media worker start (about 0.15–0.3 s, see
  above; the picture used to follow in 33 ms after the first save of a session).
  Saves pay one pipe round trip. Numbers in ROADMAP.
- The engine no longer holds the `Recorder`; anything new the engine needs from it goes
  through a command (the protocol above) — which is the "everything is a command" rule
  anyway.
- Two processes show up in Task Manager while a game runs (same executable name).
