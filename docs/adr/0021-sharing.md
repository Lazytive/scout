# 0021: Sharing (M11)

Status: accepted (2026-10-12) for the decisions below; the share server's details are
filled in as M11.6–M11.9 land. Author: Claude Opus 5.5.

## Context
KLIP_PROMPT §2 Sharing: v1 = copy a clip to the clipboard as a file, drag it out of the
library, "Export for Discord"; then a self-hostable share server (Rust/axum in Docker):
Scout uploads to a server the user configures and copies a link, with thumbnails, a
simple player page, upload tokens, size limits, never public by default. AGENTS rule 4:
no traffic but what the user starts. The user said "start M11" without further wishes,
so these are my calls (rule 8).

## Decisions
1. **Copy = the OS's "copied files"** (Windows CF_HDROP, Linux `text/uri-list` on the
   Wayland data-control clipboard, else X11), so Ctrl+V works in Discord, Explorer,
   Dolphin and chat apps. `klip_platform::copy_files` through `arboard`'s file list
   (MIT/Apache, 1Password); one clipboard handle lives for the process because on Linux
   the program that set the clipboard serves it. Engine command `clip.copy {paths}`:
   library files only; a recording still being written is refused (`busy`). Library:
   Copy in Details, the context menu and Ctrl+C; the export dialog's done state has
   "Copy file". A toast says what happened (`src/lib/toasts.svelte.ts`).
2. **Drag out = `tauri-plugin-drag`** (CrabNebula, MIT/Apache: OLE `DoDragDrop` on
   Windows, a GTK drag on Linux) with the files and the poster as the drag picture.
   Copy only, never move: a drop can't take a clip out of the library. A drag that
   starts on a selected card takes the whole selection, else just that card (as
   Explorer does). The plugin's npm wrapper has no licence field, so the app calls
   `plugin:drag|start_drag` itself (`dragFilesOut` in `src/lib/api.ts`, ten lines).
   Not tested automatically: a synthetic drag would drop the file wherever the real
   pointer is (the human checklist covers it).
3. **"Discord" on a clip** (M11.5): copy the file as is when it already fits the
   Discord limit (`export.discord_limit_mb`), is H.264/AAC MP4 and has no edits; else
   export with the Discord preset (edits applied) and copy the result.
4. **Share server** (M11.6–7): `apps/klip-share-server`, binary `scout-share`. Links are
   unlisted (128-bit random ids, no index page, `noindex`); uploads need a token stored
   hashed (`scout-share token add <name>` prints it once); each upload returns a manage
   token for deleting it; the MP4 is checked by its boxes (size from `tkhd`) and the
   poster picture comes from Scout (the server runs no decoder). The player page has
   Open Graph `og:video` tags so Discord plays the link inline, which needs a public
   HTTPS address (docs: Caddy, or Tailscale Funnel on a home server). Range requests;
   caps on upload size, total size, concurrent uploads, header size and time; optional
   expiry per upload.
5. **Scout as the client** (M11.8–9): uploads the file as is when it's H.264/AAC MP4
   without edits, else an export with the YouTube preset first (HEVC and AV1 don't play
   everywhere). `ureq` + rustls with the OS's certificate store. The upload token is
   kept in its own file beside the settings (user-only), not in `settings.json`.

## Consequences
- New dependencies: `arboard` (+ `clipboard-win`, `wl-clipboard-rs` on Linux) in
  `klip-platform`; `tauri-plugin-drag` (+ `drag`, an older `windows` crate) in the app.
- The clipboard e2e tests replace the desktop clipboard for a moment and put its text
  back, so they run with `cargo xtask e2e`, not `ci`.
- Linux paste into Discord, and drag on KDE, are checked in the Bazzite pass before M13.
