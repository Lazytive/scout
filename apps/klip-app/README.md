# klip-app

Klip's desktop app (ADR 0004, ADR 0014): a thin Tauri 2 shell over `klip-engine`, with a
Svelte 5 + TypeScript front end. All recording logic is in the engine; the app runs it,
shows it and passes the window's actions to it.

## Look

The UI follows the maintainer's Photocraft / FilmCraft apps (storytold): a pro dark UI
with a workspace header (underlined text tabs), docked panels with tab strips on
near-black gutters, dense 13 px text, a blue accent, monospace blue readouts, pill
buttons and a status bar. Every colour and size is a token in `src/app.css`; themes
(M6.7) swap the tokens.

## Pieces

- `src-tauri/src/main.rs`: starts the engine (`klip_probe::worker_main()` first: encoder
  probes re-run this executable), forwards engine events to the window
  (`klip://event`) and the tray, single instance (a second launch shows the window),
  close → tray (`app.close_to_tray`), `app.quit` from the control channel exits, and on
  exit the engine shuts down (a running recording is finalised).
- `src-tauri/src/ipc.rs`: the window's one command, `klip(cmd, args)` → the engine's
  command handler on a blocking thread; errors come back as `{code, message}`.
- `src-tauri/src/tray.rs`: tray icon drawn at runtime (grey = waiting, blue = buffering,
  red dot = recording; no image asset), tooltip, menu (save clip, start/stop recording,
  bookmark, open, quit), left click opens the window.
- `src/lib/api.ts`: `call(cmd, args)` and `onEvent(cb)`; outside Tauri in development a
  mock engine (`src/lib/mock.ts`) stands in, so the UI can be worked on in a browser
  (`npm run dev`). The mock is not in the production bundle.
- `src/lib/store.svelte.ts`: engine status (on every event and every 2 s while
  visible), storage status, activity log. `src/lib/format.ts`: display helpers.
- `src/Home.svelte` (Monitor, Info/Storage, Activity/Hotkeys panels),
  `src/lib/Panel.svelte` (panel with a tab strip), `src/App.svelte` (header, status bar).
- Icons: `src-tauri/icons/app-icon.svg` (made for Klip), sizes generated with
  `npx tauri icon src-tauri/icons/app-icon.svg -o src-tauri/icons` (desktop sizes kept).
- CSP: scripts and styles from the app only, no remote URLs.

## Build and run

```
cd apps/klip-app && npm ci && npm run build   # dist/ is embedded by the Tauri build
cargo run -p klip-app
```

`cargo xtask ci` runs eslint, svelte-check, vitest and the build before the cargo gates,
and audits npm licences from `package-lock.json` (shipped packages: permissive only).

Development and test overrides (environment): `KLIP_SETTINGS` (settings file),
`KLIP_DISCOVERY` (control-channel discovery file), `KLIP_WINDOW_POS=x,y` (create the
window there, visible and **not activated**: tests put it on a second monitor). Never
run a dev build with the real settings while someone plays: it would buffer their game
and take their hotkeys.

## Tests

- vitest: `src/lib/format.test.ts` (display helpers, which events show in Activity).
- Rust: tray icon pixels per state, tray state from events.
- Driven on 2026-10-09 (Windows, test settings, test game on monitor 2): the app found
  the test game, buffered it (nvd3d11h264enc 1280x540@60), saved a 10.8 s clip and
  started a recording through the control channel, and the window showed each step;
  `klip-cli stop` quit the app (exit 0) and the recording was finalised (27.6 s MP4).

Layer: app.
