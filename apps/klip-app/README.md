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
or Light when the OS is in light mode, before the settings arrive; the overlay uses the
same tokens.

## Pieces

- `src-tauri/src/main.rs`: starts the engine (`klip_probe::worker_main()` and
  `klip_engine::worker_main()` first: encoder probes, the recorder and thumbnail
  decoding re-run this executable, ADR 0016), forwards engine events to the window
  (`klip://event`) and the tray, single instance (a second launch shows the window),
  close → tray (`app.close_to_tray`), `app.quit` from the control channel exits, and on
  exit the engine shuts down (a running recording is finalised).
- `src-tauri/src/ipc.rs`: the window's one command, `klip(cmd, args)` → the engine's
  command handler on a blocking thread; errors come back as `{code, message}`.
- `src-tauri/src/overlay.rs` (M6.9): the overlay window (`overlay.html` →
  `src/Overlay.svelte`): transparent, always on top, click-through
  (`set_ignore_cursor_events`), `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` and shown with
  `SWP_NOACTIVATE` (`klip_platform::make_overlay`, `show_no_activate`), so the game keeps
  the focus; placed in the chosen corner of the game's monitor (the monitor under the
  game window's centre, the primary one otherwise), scaled by its DPI. Minimal: the
  "Clip saved" card for `overlay.seconds`, the thumbnail fades in when the engine's
  `thumbnail` event arrives. Full: the card, a recording dot while a recording runs,
  and the quick menu on its hotkey (the only clickable, focused state; Esc, an action
  or 15 s close it and the game gets the focus back). Off, or the game in exclusive
  fullscreen (nothing can show over it): no window. The sound (`cue.rs`: a two-note
  chime synthesised with cpal, no sound file) and the system notification
  (`tauri-plugin-notification`) follow their own switches in every mode. Settings →
  Overlay → "Show me" previews it (`overlay.preview`). Test overrides:
  `KLIP_OVERLAY_POS=x,y`, `KLIP_NO_FOCUS=1`.
- `src-tauri/src/thumb_scheme.rs` (M7): the `klipthumb` URI scheme for library
  thumbnails (`http://klipthumb.localhost/<poster|strip>/<key>?v=<version>` in
  WebView2): only cached thumbnails of library clips (the engine's `library.thumb` checks
  the key), ≤ 8 MiB, cacheable by URL; allowed in the CSP's `img-src` only.
- `src/Editor.svelte` + `src/editor/*` (M8.5, ADR 0017): the Editor workspace, opened
  from the Library (double-click, Enter, Edit). Left: format (16:9 / 9:16 with a crop
  window to drag on the preview), Split / Cut / Keep, speed buttons on the selection,
  text presets (a cue at the playhead; edit text, style, position, times), Game / Mic
  sliders, the Shorts template, the mode toggle (Advanced is M9). Centre: the preview
  (`editor/Preview.svelte`: the clip or its H.264 proxy in a `<video>` played along
  the OUTPUT timeline, segment by segment at its speed; the engine-drawn text cues on
  top; the strip picture stands in when the media can't play) with Play, the output
  timecode, Undo / Redo, and the filmstrip (`editor/Filmstrip.svelte`: the clip's
  hover-scrub tiles as the source timeline, cut parts shaded, speed badges, bookmarks,
  trim handles, drag to select, the text cues on a lane below, drag to move). Right:
  the edit's facts and Export… (`editor/ExportDialog.svelte`: presets, passes and
  progress from `export_progress`, Cancel, the result with Show in folder). State in
  `src/lib/editor.svelte.ts` (every edit is saved with `editor.project.set` after 400
  ms; `editor_media` → `convertFileSrc` for the video; `editor.text.render` → data URLs
  for the cues), timeline math in `src/lib/timeline.ts` (a port of `klip-edit`,
  checked against `crates/klip-edit/fixtures` by `timeline.test.ts`), helpers in
  `src/lib/editor.ts`, the mock in `src/lib/mock-editor.ts`. Keys: Space, S (split),
  Delete (cut the selection / remove the cue), arrows (a frame; Shift: a second),
  Home / End, Ctrl+Z / Ctrl+Y, Escape.
- `src/Library.svelte` + `src/library/*` (M7): the Library workspace. Left: Browse
  (all / starred with counts, date ranges, games, tags). Centre: search (debounced),
  sort, card size S/M/L (remembered in `localStorage`), clips grouped by day for date
  sorts; cards show the poster, and on hover the strip frame under the pointer with a
  playhead and its time (`ClipCard.svelte`), the length, bookmarks and a star; cards that
  scroll into view ask for missing thumbnails (`thumb-requests.ts`). Click / Ctrl /
  Shift select, arrows (+Shift) move, Enter opens, Del → Recycle Bin, Shift+Del →
  permanently (both confirmed), F2 renames, Ctrl+A, Esc; right-click menu. Right:
  Details (poster, name to rename with the engine's error under it, Open, Show in
  folder, star, tags, info, path, delete), several selected (star all, delete), or the
  library's totals. State and actions: `src/lib/library.svelte.ts`; pure helpers
  (names, day headings, scrub maths, selection) in `src/lib/library.ts` with vitest
  tests; the mock engine's library is `src/lib/mock-library.ts` (drawn SVG thumbnails).
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
- CSP: scripts and styles from the app only, no remote URLs. `connect-src` allows the
  asset protocol so the timeline can `fetch` a sound it already plays for its waveform
  (the asset scope is still the engine-named set from `editor_media`).

## Advanced workspace (M9.5, FilmCraft layout since M9.7)

Laid out like FilmCraft's (Premiere's) Editing workspace, because the user asked for it
to be "way more like FilmCraft" (design and comparison: `plan/advanced-redesign.md`):
top row Effect Controls / Effects / History · Program · Properties / Main clip / Export;
bottom row Project / Markers · Timeline · L/R meters. Four splitters, kept in
`localStorage` `klip.editor.layout.v2` (the old timeline height carries over).

`src/editor/advanced/`:
- `Workspace.svelte`: the layout, the Program monitor (blue timecode, length, icon
  transport, undo/redo) and the keys (Space, J/K/L, S, Ctrl+K split at the playhead
  (the selection, else every track incl. Main), V / C the Selection / Razor tool, Ctrl+A,
  Delete: a selected transition or the selected clips (Shift: ripple delete), M, [ ],
  arrows, Home/End, Ctrl+Z/Y). Selection: `ed.selected` (all) and `ed.clipId` (the one
  the panels show), `selectedClips()` / `selectClips()` in `src/lib/editor.svelte.ts`.
- `Timeline.svelte`: ONE scroller with a sticky header column and ruler (rows below the
  fold keep their clips), Premiere track headers (V2… Advanced video, V1 = Main, A1 =
  Main sound, A2… sound/music; hide/mute, rename on double-click, height by dragging a
  header's bottom edge, double-click toggles), an always-empty track above the video and
  below the sound to drop new tracks into, clip colours sampled from FilmCraft, sprite
  thumbnails on video clips, waveforms on sound clips (`Waveform.svelte`,
  `src/lib/peaks.ts`), transitions drawn on the join (hatched; click selects, drag an
  end for the length, the middle to roll the cut (`rollCut`), right-click menu, `+` on a
  touching join or at a track's first clip), a zoom bar with round handles, Ctrl/Alt+
  wheel zoom. The zoom is absolute px/s (M9.8): Fit on first open and on demand (button,
  `\`, double-click the bar), never rescaled by an edit; the lanes are at least the view
  wide. A press only moves or trims after 4 px; drags follow `window` events (a clip
  moved to another track is a new element). Shift/Ctrl-click and a marquee over empty
  lane space select (`data-clip` hit test); a selection drags together in time
  (`moveClips`). The Razor tool (blade cursor, dashed cut line) splits where clicked
  (`cutsAt` / `splitClipsAt`, Main via `timeline.split`; Shift: every track). Video
  track headers have the eye (`hidden`: no picture) and M (`muted`: no sound). V1/A1
  carry a lock (`ed.mainLocked`, closed on every open, M9.8.8). Unlocked, a piece of
  the simple cut selects (`ed.mainSel`); the first drag or Delete arranges V1
  (`arrangeMain`, M9.8.9): the cut becomes clips of the main track (`Track.main`), a
  normal track row from then on (A1 mirrors it); `useSimpleCut` goes back. Locked V1
  clips carry `data-locked` and act as empty space.
  Geometry and gesture rules: `src/lib/timeline-view.ts`.
- `EffectControls.svelte` + `HotValue.svelte`: Motion (zoom and position share keys),
  Opacity, Volume, Speed with a stopwatch each (on: keys the value at the playhead; off:
  keeps that value for the whole clip), blue drag-to-change values, ◀ ◆ ▶ and a key lane
  for the selected clip. Model: `src/lib/effect-controls.ts`.
- `Properties.svelte`: the clip, "From the clip before" (a crossfade from below into a
  track's first clip: `Transition { after_clip: FROM_BELOW }`), the transition into the
  next clip or the selected transition, the track.
- `ProjectPanel.svelte`: the library's other clips as cards (hover-scrub); drag one onto
  a video track (pointer events: Tauri's file-drop handling swallows HTML5 drag and drop
  on Windows; `addToMontageAt` = `editor.montage` + a move, one undo step) or tick and
  Add; + Sticker / Sound / Music.
- `Meters.svelte`: the sound graph's output, left and right (`SoundGraph.levels()`).
- `Canvas.svelte` + `player.ts` (the composition from `src/lib/compose.ts` drawn with
  the shared placement math; one muted `<video>` per playing layer on a wall-clock
  master; paused frames by seeking; strip tiles when nothing plays), `sound.ts`
  (`SoundGraph`: Web Audio buses, track weights as `mixer::volume_for`, volume keys,
  music ducking; `FollowSound`: Normal mode's tracks following its `<video>`),
  `EffectsPanel.svelte` (colour and ducking, or the markers), `HistoryPanel.svelte`;
  `src/editor/NormalTools.svelte` is shared by both modes.

No browser UI (M9.8.1): `src/lib/browser-ui.ts` cancels the WebView's context menu
outside text fields and its accelerators (print, save as, find, reload, view source,
devtools, back/forward) in the app's windows; the browser mock keeps them.

Media elements use `crossorigin="anonymous"` (Tauri's asset protocol answers with the
window's origin), so Web Audio hears them and the canvas stays readable. The project's
files and every placed clip's media come through `editor_media` with
`what: proxy | audio | files` (`src/lib/api.ts`).

Test hook: `window.__klipTest.parity(out_ms, width, images?)` (only while the Advanced
workspace is open) draws the canvas at the size `editor.frame` returns for the same
time and gives the RGB PSNR between them. To drive the real app from a script, start it
with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333` (plus the
off-screen variables below) and evaluate through the Chrome DevTools Protocol
(`http://127.0.0.1:9333/json`, `Runtime.evaluate`, `Page.captureScreenshot`).

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
`KLIP_DISCOVERY` (control-channel discovery file), `KLIP_START_PAGE=home|library|settings`
(the page the window opens on; a Klip on its own `KLIP_SETTINGS` skips the
single-instance hand-over, so it runs beside the user's Klip instead of showing it; give
it its own `WEBVIEW2_USER_DATA_FOLDER` too), `KLIP_WINDOW_POS=x,y` (create the
window there, visible and **not activated**: tests put it off-screen, e.g. `12000,200`,
and capture it with `tools/window-shot.ps1`). Never
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
- Rust: tray icon pixels per state, tray state from events, overlay corners and prefs,
  the chime (length, level, no clicks).
- e2e `src-tauri/tests/overlay.rs` (everything off-screen and silent): the pop-up shows
  3–11 ms after the request, stays 2.00 s (setting: 2 s), is never the foreground
  window, and the test game's frame pacing at 120 fps is unchanged (2026-10-09:
  intervals p99 8.84 ms without, 8.77 ms with the pop-ups; max 9.4 / 10.0 ms). Off-screen
  means the overlay never covered the game, so the effect of a window over the game on
  VRR/independent flip is not measured here (human checklist).
- The card with a real thumbnail was captured off-screen (`tools/window-shot.ps1`).
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
