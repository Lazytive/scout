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
| `cargo xtask bench` | What Scout costs a game (fps off/on, CPU, RAM, save time); needs an idle GPU: ask the human |
| `cargo xtask layers` | Dependency-direction and `unsafe` placement check |
| `cargo xtask audit` | Licence/asset audit (GStreamer plugin denylist, ATTRIBUTION.md) |
| `cargo run -p klip-testgame -- --help` | Test game with barcoded frame counter |
| `cargo run -p klip-cli -- --help` | Recorder CLI (`daemon` runs the app's engine headless) |
| `cd apps/klip-app && npm ci && npm run build` then `cargo run -p klip-app` | Desktop app (see its README for the `KLIP_SETTINGS`/`KLIP_DISCOVERY`/`KLIP_WINDOW_POS` test overrides) |
| `npm run dev` in `apps/klip-app` | UI in a browser with a mock engine |

## Environment (Windows dev machine)

- Rust stable MSVC (pinned in `rust-toolchain.toml`), VS 2022 Build Tools.
- GStreamer 1.28 MSVC (runtime + devel) in `C:\Program Files\gstreamer\1.0\msvc_x86_64`;
  its `bin` must be on `PATH` and `PKG_CONFIG_PATH` must point at its `lib\pkgconfig`.
  `cargo xtask` sets both for child processes if they're missing.
- FFmpeg on `PATH` for test oracles only.

## Environment (Linux: the same PC dual-booted into Bazzite)

- Bazzite's `/usr` is read-only, so everything builds in the `klip-dev` distrobox
  (Fedora 44, created with `--nvidia`): GStreamer 1.28 devel + plugins (good, bad-free,
  libav, openh264, pipewire), full FFmpeg from RPM Fusion (test oracle), rustup 1.99 and
  cargo-deny in the shared `~/.cargo`. Run commands as
  `distrobox enter klip-dev -- bash -c 'cd <repo> && CARGO_TARGET_DIR=~/.cache/klip-target cargo …'`
  (the repo sits on the Windows NTFS drive; keep build output on the Linux disk).
- The container shares the session bus, PipeWire, Wayland and the GPU, so portal capture
  and NVENC work from inside it.
- e2e: the test game goes fullscreen on `KLIP_E2E_OUTPUT` (default `secondary` = DP-3
  here); the portal token for tests is `~/.local/state/klip-e2e/portal-<output>.token`.
  Without it the KDE dialog appears: the human must pick that monitor (not "Full
  Workspace").
