# klip-app

Klip's desktop app (ADR 0004, ADR 0014): a thin Tauri 2 shell over `klip-engine`, with a
Svelte 5 + TypeScript front end. All recording logic is in the engine; the app runs it,
shows it and passes the window's actions to it.

## Look

The UI follows the maintainer's Photocraft / FilmCraft apps (storytold): a pro dark UI
with a workspace header (underlined text tabs), docked panels with tab strips on
near-black gutters, dense 13 px text, a blue accent, monospace blue readouts, pill
buttons and a status bar. Every colour and size is a token in `src/app.css`.

Themes (`src/lib/themes.ts`, M6.7): **Follow system** (the default: Light or Dark from
the OS, live), **Dark** (FilmCraft greys), **Graphite** (Photocraft greys), **Light**,
**OLED black**. The sun button in the header (as in Photocraft) picks one; it is saved as
`appearance.theme` and applied at once, also when another client changes the setting
(`settings_changed`). The native title bar follows (`setTheme`). `app.css` paints Dark,
or Light when the OS is in light mode, before the settings arrive; the overlay (M6.9)
will use the same tokens.

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
- `src/Settings.svelte`: the Settings workspace (categories left, the page right, like
  Photoshop's preferences): General (start with Windows, tray, theme, local control,
  run setup again), Recording (buffer with the RAM estimate, preset or custom numbers,
  encoder from `encoders.list`, cursor), Audio (game sound, voice apps, microphone with
  device list and a live test meter, music excludes, tracks, bitrate), Hotkeys (rows of
  action / clip length / keys; key capture pauses the engine's hotkeys; the engine's
  "already used by another app" shows per row), Games (buffer mode, my games, running
  programs to add, never list, per-game buffer / quality / sound / mic), Storage
  (folders with the system picker, size limit, Recycle Bin or delete, low-disk warning,
  usage), Overlay. Every control saves at once with a strict `settings.set`; a
  rejection shows its reason under the field (`store.save`, `fieldError`).
- `src/FirstRun.svelte`: shown while `app.first_run_done` is false: your PC (GPU and the
  encoder Klip will use), quality + buffer with RAM estimate, clip hotkey, microphone
  with test meter, clip folder, pop-up style; each step saves as it goes; Skip/Finish
  set `first_run_done`.
- `src/lib/forms/`: Row, Toggle (blue square checkbox), Select, NumberField, Segmented
  (pill radio group), KeyCapture, ListEditor, FolderField, Meter. `src/lib/settings.ts`:
  the settings types and pure helpers (merge patches, error paths, key capture).
- Plugins: `tauri-plugin-dialog` (folder picker), `tauri-plugin-autostart` (applied only
  when `app.autostart` changes, so the OS's own startup-apps choice isn't overridden at
  every start).
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

UI work with hot reload: `npm run dev` in `apps/klip-app`, then
`cargo run -p klip-app --no-default-features` (without the default `custom-protocol`
feature the app loads the dev server at `devUrl` instead of the embedded `dist/`).

`cargo xtask ci` runs eslint, svelte-check, vitest and the build before the cargo gates,
and audits npm licences from `package-lock.json` (shipped packages: permissive only).

Development and test overrides (environment): `KLIP_SETTINGS` (settings file),
`KLIP_DISCOVERY` (control-channel discovery file), `KLIP_WINDOW_POS=x,y` (create the
window there, visible and **not activated**: tests put it on a second monitor). Never
run a dev build with the real settings while someone plays: it would buffer their game
and take their hotkeys.

## Tests

- vitest: `src/lib/format.test.ts` (display helpers, which events show in Activity);
  `src/lib/themes.test.ts`: every text/background pair the UI uses reads at WCAG AA
  (4.5:1) and every outline/focus/state colour stands out at 3:1, in all four themes
  (88 checks; it caught Light's warning orange on the status bar at 4.3:1); `app.css`
  matches Dark; setting → theme mapping.
- vitest `src/lib/settings.test.ts`: merge patches, error paths, key names, key capture
  (order, waiting on modifiers, Esc/Backspace, unsupported keys), engine errors per row.
- Rust: tray icon pixels per state, tray state from events.
- M6.8 driven in the browser with the mock (every Settings page; custom quality fields;
  live meter; a duplicate hotkey rejected on its row; the first-run steps to Finish) and
  in the real app (embedded UI captured off-screen with `tools/window-shot.ps1`; the
  first-run setup was completed by hand in the real window and every step was saved).
  Real engine answers: `encoders.list` 7 working H.264 encoders on the RTX 4080 SUPER
  (from the cache, 4 ms), 5 inputs / 3 outputs, estimate 231 MB for 2 min Balanced with
  game audio.
- Driven on 2026-10-09 (Windows, test settings, test game on monitor 2): the app found
  the test game, buffered it (nvd3d11h264enc 1280x540@60), saved a 10.8 s clip and
  started a recording through the control channel, and the window showed each step;
  `klip-cli stop` quit the app (exit 0) and the recording was finalised (27.6 s MP4).
  M6.7: `klip-cli call settings.set '{"appearance":{"theme":"light"}}'` switched the
  running window (and its title bar) to Light; all themes checked in the browser too.
  (Those two runs loaded the UI from the Vite dev server, which happened to be running:
  debug builds did that until M6.8 made the embedded `dist/` the default. The UI source
  and the engine were the same.)

Layer: app.
