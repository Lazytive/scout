# xtask

`cargo xtask <task>` (alias in `.cargo/config.toml`).

| Task | Does |
|---|---|
| `ci` | `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`, `cargo deny check`, `layers`, `audit` |
| `layers` | Every crate classified (pure / engine / app); pure crates have no GStreamer/OS/UI deps and no higher-layer normal deps; engine crates no UI/app deps; the `unsafe` keyword only in `crates/klip-platform` |
| `audit` | Denied encoder names (read from `klip-probe/src/policy.rs`) absent from product source; bundled GStreamer plugins (`dist/gstreamer/lib/gstreamer-1.0`, from M13) contain no GPL plugin; every file in `assets/` listed in `ATTRIBUTION.md`; npm licence audit once `apps/klip-app` exists (fails loudly until it's implemented) |
| `e2e` | Release build of the test game and CLI, then all `#[ignore]`d hardware tests, one at a time |

On Windows, if `GSTREAMER_1_0_ROOT_MSVC_X86_64` is set, child processes get GStreamer's
`bin` on `PATH` and `PKG_CONFIG_PATH` set, so a shell opened before the GStreamer install
still works. `e2e` needs ffmpeg/ffprobe on `PATH` (test oracles only).
