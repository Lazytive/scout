# 0023: The setup in Scout's own look (M13.4)

Status: accepted (2026-10-21). Author: Claude Fable 5.1 (the user: "I want it to look way
more like the actual app").

## Context
The Windows installer is NSIS through Tauri's bundler (M13), signed for the updater (ADR
0022). Branded (M13.3) it is still a Win32 wizard: no installer toolkit draws the app's
UI. The user wants the setup to look like Scout. The updater runs whatever `latest.json`
names with NSIS's switches (`/P /UPDATE`, `/R`, `/ARGS …`) and never looks at it.

## Decisions
1. **A setup app wraps the installer.** `apps/klip-setup` (binary `scout-setup`, shipped
   as `Scout_<version>_x64-setup.exe`) is a Tauri window drawn from the app's own front
   end: `apps/klip-app/setup.html` → `src/Setup.svelte`, the same `app.css` tokens, title
   bar and icon as Scout. It carries the NSIS installer inside it (`build.rs` embeds the
   file `cargo xtask bundle` names in `SCOUT_NSIS_PAYLOAD`), writes it to
   `%TEMP%\scout-setup-<pid>\` when the user clicks Install, runs it silently (`/S`, the
   update flag as given, `/D=<folder>` last, unquoted as NSIS wants), shows progress from
   the bytes arriving in the folder against what a whole install holds
   (`SCOUT_INSTALL_BYTES`, measured at bundle time; the bar stops at 95 % until the
   installer ends), then offers "Open Scout". The engine is never involved.
2. **The same switches as NSIS.** `/S` installs with no window at all (the setup needs no
   WebView2 then); `/P`, what the updater passes, shows the window but needs no click and
   opens Scout at the end when `/R` was given, with `/ARGS` handed to Scout; `/D=` picks
   the folder; `/UPDATE` is passed through. So the updater's download is this exe and
   nothing in ADR 0022 changes.
3. **Without WebView2 the wizard inside runs instead** (`tauri::webview_version()` fails):
   the branded NSIS wizard of M13.3, which installs WebView2 as Tauri's template does.
   The wizard is also what testers' `--full-logs` builds remain.
4. **Per-user by default** (`%LOCALAPPDATA%\Scout`): no elevation, so the updater can
   install passively. The NSIS inside keeps the uninstaller, the Programs entry and the
   Start menu shortcut.
5. **`cargo xtask bundle`** renames Tauri's installer to `Scout_<version>_x64-setup-engine.exe`,
   builds `klip-setup` with it embedded, puts the result in the installer's place, and
   with `--sign` signs that file with `tauri signer sign` (the engine's `.sig` is dropped)
   before `latest.json` is written. The release workflow needs no change.

## Consequences
- Two Tauri binaries from one front end; the setup is ~5 MB on top of the installer.
- The setup's progress is an estimate: NSIS says nothing while silent.
- The window follows the OS's light or dark scheme like the app before its settings load.
- Not done: a branded uninstaller (NSIS's own, with the M13.3 images).
