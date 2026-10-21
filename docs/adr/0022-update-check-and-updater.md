# 0022: Update check and updater (M13)

Status: accepted (2026-10-21) for the decisions below; the first signed release confirms
them. Author: Claude Fable 5.1 (the user: "Start on the update check and updater for
Release 0.1, using GitHub Releases").

## Context
KLIP_PROMPT asks for Tauri's updater with signed update manifests, Windows installers
through Tauri (NSIS) and Flatpak as the Linux package. AGENTS rule 4: the only network
traffic is the update check, which can be switched off, and sharing the user starts.
Rule 8: publishing, releasing and anything that costs money (Authenticode signing) are
the human's. No release exists yet; `cargo xtask bundle` makes the NSIS installer with
Scout's own GStreamer (M13 start). The repository is public on GitHub since 2026-10-21,
so GitHub Releases can host the installers for free.

## Decisions
1. **The manifest is Tauri's updater JSON**, `latest.json`, attached to each GitHub
   release and read at the fixed address
   `https://github.com/Lazytive/scout/releases/latest/download/latest.json` (GitHub's
   redirect to the newest release's asset: no API, no token, no rate limit). It holds
   `version`, `notes`, `pub_date` and `platforms."windows-x86_64"` with the installer's
   `url` and minisign `signature`. Linux has no entry until a Linux package exists.
2. **The check is Scout's own** (`apps/klip-app/src-tauri/src/updater.rs`): one GET of
   the manifest with the updater plugin's HTTP client, `reqwest`, in blocking mode (the
   OS's own TLS and certificate store: schannel on Windows, OpenSSL on Linux; 10 s, at
   most 64 KiB, `User-Agent: Scout/<version>`), the version compared with semver (newer only; a
   pre-release counts only against a pre-release), and the result as the event
   `update_status` (`state`, `current`, `version`, `notes`, `date`, `url`, `progress`,
   `error`, `checked_ms`, `installs`). The window's `klip` IPC answers `update.status`,
   `update.check`, `update.install` and `update.open` (the release page) itself, before
   the engine sees a command: the engine stays free of network code (ADR 0014's
   layering) and the CLI and daemon don't update themselves, so the control channel has
   no update commands.
3. **When**: automatically only while `app.update_check` is true (default true,
   Settings → General with the hint of what is sent), 30 s after the app starts and
   every 24 h while it runs, never while a recording is being written (that tick waits
   for the next), and never when `KLIP_UPDATE_URL=off` (tests). `KLIP_UPDATE_URL` points
   the check at another manifest (a loopback server in tests). "Check now" is the user's
   own request and always runs.
4. **Install, Windows only, through `tauri-plugin-updater`** (MIT/Apache): it downloads
   the installer named in the manifest, verifies its minisign signature against the
   public key in `tauri.conf.json` before anything runs, and starts the NSIS installer in
   passive mode; the app exits for it, after the engine was shut down (the recorder
   stopped, a running recording finalised) through the plugin's `on_before_exit`. Only on
   the user's click ("Install and restart" in the update prompt or in Settings →
   General), and refused while a recording runs (`busy`). On Linux the notice says "Get
   it on GitHub" and opens the release page; Flatpak updates itself (M13).
5. **Signing**: one minisign key pair for the project (`npx tauri signer generate`). The
   public key lives in `tauri.conf.json` (`plugins.updater.pubkey`); the private key in
   the maintainer's `~/.tauri/scout.key` and, for the release workflow, in the repo's
   `TAURI_SIGNING_PRIVATE_KEY` secret (and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if it
   has one). `cargo xtask bundle --sign` builds with `createUpdaterArtifacts`, so
   `tauri build` writes the installer's `.sig`, and then writes `latest.json` beside it.
   The release workflow (`.github/workflows/release.yml`, on tags `v*`) does the same on a
   Windows runner and attaches the installer, its `.sig` and `latest.json` to a **draft**
   release; the human publishes (rule 8). A lost key means a new pair and a manual
   reinstall for everyone, since old installs verify against the old public key: keep it.
   No Authenticode signing (money: ask first).
6. **Privacy**: the check sends nothing but the request itself; GitHub sees the IP
   address and the user agent, no identifier. Off means no request, ever. The result and
   the time of the last check live in memory only; nothing is written to disk.
7. **UI**: while an update is known, the title bar shows an "Update" button next to the
   recorder (as the restart icon does), which opens the prompt (like `RestartPrompt`):
   the version, the release notes, "Install and restart" (Windows) or "Get it on GitHub"
   (Linux), and "Later" (the button stays). Download progress shows in the prompt.
   Settings → General has the switch, the version running, the last check and "Check
   now". A failed automatic check stays quiet (being offline is normal); a failed manual
   check says why.

## Consequences
- One HTTP client in the app, the plugin's `reqwest`, for the check (blocking) and the
  download, on the OS's TLS (no bundled root certificates; the plugin without its `zip`
  feature, since Scout extracts no archive). The check is still Scout's own because it
  must work on Linux, where the plugin has nothing to install; the plugin's verified
  install path is not worth rewriting.
- A release is three artifacts (installer, `.sig`, `latest.json`) and two secrets; the
  README's release steps say so.
- Not done: delta updates, update channels, "skip this version", Linux packages.
