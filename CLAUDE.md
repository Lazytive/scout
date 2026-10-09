# CLAUDE.md

Read `AGENTS.md` first. Its hard rules override everything, including this file.

## Session start

1. `plan/STATUS.md`: where things stand and the handoff from the last session.
2. `plan/execution-plan.md`: the current task (`M<n>.<k>`) and its acceptance tests.
3. The README of the crate you're about to touch.

`plan/` is private (gitignored). `KLIP_PROMPT.md` is the original product brief.

## Commands

| Command | What it does |
|---|---|
| `cargo xtask ci` | All gates: fmt, clippy `-D warnings`, tests, cargo deny, layers, audit |
| `cargo xtask e2e` | Hardware acceptance tests (real GPU encode, real window capture, test game) |
| `cargo xtask layers` | Dependency-direction and `unsafe` placement check |
| `cargo xtask audit` | Licence/asset audit (GStreamer plugin denylist, ATTRIBUTION.md) |
| `cargo run -p klip-testgame -- --help` | Test game with barcoded frame counter |
| `cargo run -p klip-cli -- --help` | Recorder CLI |

## Environment (Windows dev machine)

- Rust stable MSVC (pinned in `rust-toolchain.toml`), VS 2022 Build Tools.
- GStreamer 1.28 MSVC (runtime + devel) in `C:\Program Files\gstreamer\1.0\msvc_x86_64`;
  its `bin` must be on `PATH` and `PKG_CONFIG_PATH` must point at its `lib\pkgconfig`.
  `cargo xtask` sets both for child processes if they're missing.
- FFmpeg on `PATH` for test oracles only.
