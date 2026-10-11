# 0020: The app is called Scout (was Klip)

Status: accepted (2026-10-11). Author: Claude Opus 5.5.

## Context
"Klip" was a working name (AGENTS rule 8, KLIP_PROMPT). On 2026-10-11 the user chose
**Scout** and asked for the rename before M13 Release 0.1. The user's conditions: the
app isn't commercial, so only the GitHub name matters (no domains, stores or trademarks
to check; `Lazytive/scout` was free). The rename must never lose a clip: the user has
a library in `Videos\Klip` and settings and themes in `%APPDATA%\Klip`.

## Decisions
1. **What users see says Scout**: window and overlay titles, tray, notifications, the
   first-run setup, settings texts, the CLI's help and errors, docs, the Tauri
   `productName` (installer and bundle name). History stays as written: ADRs 0001–0019,
   older CHANGELOG entries, `KLIP_PROMPT.md` (the original brief).
2. **App id `io.github.lazytive.Scout`** (Tauri identifier and the Linux portal app id):
   the reverse-DNS form for an app hosted on GitHub, which Flathub accepts (M13 Flatpak).
   It was `dev.klip.Klip`. Consequences: a new WebView2 profile (the UI's first-paint
   cache starts empty once), the single-instance lock is new (quit Klip before starting
   Scout), on Linux the portals ask again once (shortcuts, screen-cast restore token).
   The hidden Linux desktop entry of the old id is removed.
3. **Internal names stay**: the `klip-*` crates and binaries (`klip-app.exe`,
   `klip-cli`), the `KLIP_*` test and development overrides, the `.klip` folder inside
   the clip folder (projects, history, proxies, thumbnails), the `klipthumb:` scheme,
   the `klip` Tauri command and `klip://event`, the `CaptureApi::Klip` capture path, the
   `klip.look.v1` storage key. Renaming them would touch every test and file format for
   nothing a user sees; they may change later with a reason.
4. **New file names**: clips `Scout_<time>.mp4`, recordings
   `Scout_Recording_<time>.mp4`; the library still shortens `Klip_…` names to "Clip
   18:22:01". Theme files `*.scout-theme` with `"scout_theme": 1`; `*.klip-theme` files
   and the `klip_theme` key still import.
5. **Migration at the first start of Scout** (`klip_engine::migrate`, run by the app and
   `klip-cli daemon` before the engine reads its settings; never with a custom settings
   file): when `<config>/Scout/settings.json` doesn't exist and `<config>/Klip/settings.json`
   does, the settings and the custom themes are **copied** (the Klip folder stays, so an
   old build still starts unchanged). Then, only if the settings don't name a clip
   folder, `Videos/Klip` is **moved** to `Videos/Scout` with a single directory rename:
   all or nothing, no copy, no delete. If it can't be renamed (a file in use, another
   drive, `Videos/Scout` exists), Scout keeps using `Videos/Klip` by writing it into its
   settings as the clip folder. A chosen clip or recordings folder is never touched.
   Clip paths inside the library are relative to the folder (the `.klip` index moves
   with it), so the library, projects and thumbnails follow. The encoder cache is not
   copied (it is re-probed once); the control channel's discovery file is per run.
6. **Start with the OS**: if Klip's entry exists (Windows `Run` value or Linux autostart
   file named Klip), it is removed and Scout's is switched on instead, so the OS doesn't
   keep a Klip entry the Scout setting can't switch off.
7. **The GitHub repo** is renamed `Lazytive/klip` → `Lazytive/scout` only with the
   user's OK (AGENTS rule 8: publishing); GitHub redirects the old URL.

## Consequences
- The user must quit Klip (tray → Quit) before the first start of Scout; otherwise two
  apps run (different single-instance ids) and the clip folder may be in use (then it
  is kept, decision 5).
- Tests: `klip_engine::migrate` (move, kept when `Videos/Scout` exists or a clip is
  open without sharing, a chosen folder untouched, once only), the theme key's old name,
  the library's old file names.
