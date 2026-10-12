# Scout

A free, open-source game clipper for Windows and Linux: an always-on replay buffer that
saves the last moments of your game to a plain MP4 with one key, using your GPU's hardware
encoder (NVIDIA, AMD, Intel). No account, no telemetry. (Called Klip until 2026-10-11:
the crates and some internal names still are.)

**Status: early development, before the first release.** What exists today: the desktop
app (tray, overlay, settings, game detection, a title bar with the recorder in it) and a
CLI around one engine; the replay buffer and full-session recording with the GPU's encoder
(NVENC; AMD AMF checked on a tester's PC; Intel Quick Sync untested) or the OS's, and audio (game, apps, voice chat, microphone) in sync with
the video; the Library (the start page); the Editor in Normal mode (trim, cut, speed,
text, 16:9 / 9:16, volumes) and Advanced mode (tracks, keyframes, montages of several
clips); export presets with a Discord size fit; themes; copy a clip as a file, drag it
out, "Discord" in one click, and a self-hostable share server. Windows is used daily;
Linux (KDE Plasma, ScreenCast portal + PipeWire) builds and passes every gate in CI, and
gets its hardware pass before the release.
See [ROADMAP.md](ROADMAP.md) for what works and what's measured.

## Anti-cheat safety

Scout never injects into games, hooks their rendering, or reads their memory. It captures
through OS APIs only (Windows Graphics Capture and WASAPI; on Linux the ScreenCast portal +
PipeWire).

## Privacy

Recordings stay on your computer. When Scout records a game's audio it also listens to the
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

The desktop app needs Node.js for its front end:

```
cd apps/klip-app && npm ci && npm run build && cd ../..
cargo run -p klip-app
cargo xtask bundle      # Windows installer with its own GStreamer (target/release/bundle/nsis)
```

## Building (Linux)

Tested on Fedora 44 (also inside a distrobox on Bazzite). The packages CI installs:

```
sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-44.noarch.rpm
sudo dnf install --allowerasing --enablerepo=fedora-cisco-openh264   gcc gcc-c++ make clang clang-devel pkgconf-pkg-config   gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-base   gstreamer1-plugins-good gstreamer1-plugins-bad-free gstreamer1-plugin-libav   gstreamer1-plugin-openh264 pipewire-devel pipewire-gstreamer alsa-lib-devel dbus-devel   webkit2gtk4.1-devel gtk3-devel libsoup3-devel javascriptcoregtk4.1-devel   libappindicator-gtk3-devel librsvg2-devel libxdo-devel openssl-devel   wayland-devel libxkbcommon-devel ffmpeg
```

Then Rust via rustup (the version in `rust-toolchain.toml` installs itself), Node.js, and
the same commands as on Windows. Screen capture goes through the desktop's ScreenCast
portal: the first recording asks which screen to share.

## Licence

MIT OR Apache-2.0, at your option. GStreamer is used as a dynamically linked LGPL library.
Third-party assets are listed in [ATTRIBUTION.md](ATTRIBUTION.md).
