# Klip

A free, open-source game clipper for Windows and Linux: an always-on replay buffer that
saves the last moments of your game to a plain MP4 with one key, using your GPU's hardware
encoder (NVIDIA, AMD, Intel). No account, no telemetry. "Klip" is a working name.

**Status: early development.** What exists today is a recorder CLI on Windows with NVENC
and audio (game, apps, microphone) in sync with the video.
See [ROADMAP.md](ROADMAP.md) for what works and what's measured.

## Anti-cheat safety

Klip never injects into games, hooks their rendering, or reads their memory. It captures
through OS APIs only (Windows Graphics Capture and WASAPI; on Linux the ScreenCast portal +
PipeWire).

## Privacy

Recordings stay on your computer. When Klip records a game's audio it also listens to the
whole system mix, only to measure timing: that audio is analysed in memory for a few
seconds and never written anywhere. Music players are left out of system audio by
default, and the microphone is off unless you turn it on.

## Building (Windows)

Prerequisites: Rust (MSVC toolchain, see `rust-toolchain.toml`), Visual Studio 2022 Build
Tools (C++), GStreamer ≥ 1.24 MSVC runtime + development installer. FFmpeg is only needed
to run the clip-check tests.

```
cargo xtask ci          # all gates
cargo xtask e2e         # hardware acceptance tests (needs a GPU and a desktop session)
cargo run -p klip-cli -- record --buffer 120s
cargo run -p klip-cli -- clip --last 30s
```

## Licence

MIT OR Apache-2.0, at your option. GStreamer is used as a dynamically linked LGPL library.
Third-party assets are listed in [ATTRIBUTION.md](ATTRIBUTION.md).
