# klip-app

Scout's desktop app (ADR 0004, ADR 0014): a thin Tauri 2 shell over `klip-engine`, with a
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

Themes v1 (M10, ADR 0019, `src/lib/appearance.ts`): **Settings → Appearance**
(`settings/Appearance.svelte`) has theme cards (built-in and custom, each drawn in its own
colours), an **accent colour** (Theme's own, 8 swatches or any colour; the pressed shade,
accent text, text on the accent and focus ring are derived to keep AA where the colour
allows), **text size** (90/100/115/130 % via `--text-scale`: every `font-size` is a token
or `calc(… * var(--text-scale))`), **density** (Comfortable = the standard look,
Compact = `:root[data-density='compact']` with tighter `--space-*`, bars and
`--control-h`), **motion** (System / Reduced / Full: `data-motion='reduce'` zeroes every
transition and animation) and a **contrast check** (the `textPairs`/`uiPairs` below WCAG
AA for the look on screen). **Custom themes** (`theme.*` commands): New theme (a copy of
the current one, put in use), Import… (`*.scout-theme`; what was repaired is listed),
Edit, Export…, Delete (asks first; the theme in use falls back to Follow system). The
editor (`settings/ThemeEditor.svelte`) previews every change on the whole app
(`app.themePreview`) until Save or Cancel: name, base theme, 21 colours in five groups
(changed ones bold with ↺), corner roundness 0–16 px, a font by name (suggestions are the
common fonts found on this PC; "not installed" is detected), a background picture (saved
at once; dim 0–90 %, panel opacity 60–100 %) and the live contrast check. With a picture
the window backdrop shows it and the chrome, status bar, panels and viewer turn
translucent; menus and inputs stay opaque. The header's sun menu lists custom themes and
opens Appearance. The last look is cached in `localStorage` (`klip.look.v1`) and applied
in `main.ts` before mounting, so a custom theme doesn't flash Dark. The overlay follows
colours, accent, corners, font and motion (not text size, density or the picture: the
app sizes its window).

Motion (2026-10-21): every entrance is opacity and transform only, timed from
`--motion` (`--motion-enter` = 1.5× for pages, cards, menus and dialogs; `--ease-out`),
so Reduced motion turns all of it off. The shared keyframes live in `app.css`
(`fade-in`, `fade-rise`, `pop-in`, `grow-x`): a workspace fades and rises in when it
opens (`main > :first-child`; each page's root mounts anew), a tab's underline grows
in, menus and dialogs pop in (a menu from the corner it hangs from) over a fading
scrim. Local ones: the Library's cards (a stagger and the fresh glow, below), the
star's pop, the recorder bar's REC pulse and Starting blink.

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
  `KLIP_OVERLAY_POS=x,y`, `KLIP_NO_FOCUS=1`. Linux: made click-through when shown, not
  when created (a GTK window has no surface before it is first shown, and tao aborts
  the app otherwise); on an NVIDIA GPU `main` sets `WEBKIT_DISABLE_DMABUF_RENDERER=1`
  unless the user set it (WebKitGTK's DMA-BUF renderer ends the app with Wayland
  protocol error 71 when the overlay shows: KDE Plasma 6.7, driver 615). On Wayland it
  is a layer-shell surface when gtk-layer-shell is installed
  (`klip_platform::overlay_layer_init`/`overlay_layer_place`): the card in its corner
  and the menu in the middle of the active output, above fullscreen games; the menu
  takes the keyboard while shown (for Esc). Without it the window shows where the
  compositor puts it (KDE: the middle). Opaque on Linux, with the card filling the
  window: a see-through WebKitGTK view drew only the animated timer bar.
- `src-tauri/src/media_server.rs` (Linux): the preview's video, sound and pictures over
  loopback HTTP. WebKitGTK plays `<video>`/`<audio>` only from http(s), file and blob
  URLs (its GStreamer source has no element for Tauri's `asset:` scheme: "FormatError"
  before a byte is read; WebKitGTK 2.54), so on Linux `fileUrl` (`api.ts`) asks
  `media_base` once and points at `http://127.0.0.1:<port>/<token>/<percent-encoded
  path>`. It serves exactly the asset protocol's scope (the engine-named files of
  `editor_media`, files picked in a dialog); a 128-bit token in the path, the same 404
  for a wrong token, a path outside the scope or a missing file; GET/HEAD, one byte
  range, `Connection: close`, at most 32 connections, 16 KiB of headers, timeouts; CORS
  only for the app's own origins. The CSP allows `http://127.0.0.1:*` for images,
  media and `fetch`. Windows keeps the asset protocol (WebView2 plays it).
- `src-tauri/src/updater.rs` (M13.2, ADR 0022): the update check and the updater. One
  GET of the newest release's `latest.json` on GitHub (the plugin's `reqwest` in blocking mode,
  on the OS's own TLS and certificate store, 10 s, 64 KiB at most, `User-Agent:
  Scout/<version>`, nothing else), compared with semver (a pre-release only replaces a pre-release): 30 s after
  the start and daily while `app.update_check` is on and no recording is being
  written, and on "Check now". The window's `update.status|check|install|open` go
  through `klip` but are answered here (the engine has no network code); the
  `update_status` event carries the whole state. Install (Windows):
  `tauri-plugin-updater` fetches the installer the manifest names, verifies its
  minisign signature against `plugins.updater.pubkey` in `tauri.conf.json`, runs it
  passively and exits the app after the engine shut down (`on_before_exit`); refused
  while a recording runs (`busy`). Linux: "Get it on GitHub". UI:
  `src/lib/UpdatePrompt.svelte` (version, notes, progress, Install and restart /
  Later), the title bar's Update button (`library/RecorderBar.svelte`) and Settings →
  General (the switch, the state line, Check now); wording in `src/lib/update.ts`; the
  mock finds a 0.2.0 and pretends to install it.
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
  The first dozen cards of a group fade in one after another (`--i` on the card) and a
  clip saved in the last 30 s arrives with an accent glow (`lib.fresh` from
  `clip_saved`, `isFresh`). An **ASCII water** behind the grid is an animation
  experiment, not for the release (`src/lib/ascii-water.ts`,
  `src/library/AsciiWater.svelte`): off unless Settings → Debug → Library background
  turns it on (tester builds and the mock only; kept in `localStorage` as
  `klip.library.water`). Three WebGL2 passes, ripples the pointer and random drops stir,
  a light per glyph cell, glyphs from an atlas, in the theme's text colour at low alpha;
  it moves only while the window has focus and is visible, draws one still frame with
  reduced motion, and bows out (a plain grid) without WebGL2 float render targets. The
  mock's `clip.save` adds its clip to the mock library (`mock.saved`).
- `src-tauri/src/tray.rs`: tray icon drawn at runtime (grey = waiting, blue = buffering,
  red dot = recording; no image asset), tooltip, menu (save clip, start/stop recording,
  bookmark, open, quit), left click opens the window.
- `src/lib/api.ts`: `call(cmd, args)` and `onEvent(cb)`; outside Tauri in development a
  mock engine (`src/lib/mock.ts`) stands in, so the UI can be worked on in a browser
  (`npm run dev`). The mock is not in the production bundle.
- `src/lib/store.svelte.ts`: engine status (on every event and every 2 s while
  visible), storage status, activity log. `src/lib/format.ts`: display helpers.
- `src/Settings.svelte`: the Settings workspace (categories left, the page right, like
  Photoshop's preferences): General (start with Windows, tray, local control,
  run setup again), Appearance (M10, see Look), Recording (buffer with the RAM
  estimate, preset or custom numbers,
  encoder from `encoders.list`, cursor), Audio (game sound, voice apps, microphone with
  device list and a live test meter, music excludes, tracks, bitrate), Hotkeys (rows of
  action / clip length / keys; key capture pauses the engine's hotkeys; the engine's
  "already used by another app" shows per row), Games (buffer mode, my games, running
  programs to add, never list, per-game buffer / quality / sound / mic), Storage
  (folders with the system picker, size limit, Recycle Bin or delete, low-disk warning,
  usage), Overlay. Every control saves at once with a strict `settings.set`; a
  rejection shows its reason under the field (`store.save`, `fieldError`).
- `src/FirstRun.svelte`: shown while `app.first_run_done` is false: your PC (GPU and the
  encoder Scout will use), quality + buffer with RAM estimate, clip hotkey, microphone
  with test meter, clip folder, pop-up style; each step saves as it goes; Skip/Finish
  set `first_run_done`.
- `src/lib/forms/`: Row, Toggle (blue square checkbox), Select, NumberField, Segmented
  (pill radio group), KeyCapture, ListEditor, FolderField, Meter. `src/lib/settings.ts`:
  the settings types and pure helpers (merge patches, error paths, key capture).
- Plugins: `tauri-plugin-drag` (M11.4: drag clips out as files; called as
  `plugin:drag|start_drag` from `dragFilesOut` in `src/lib/api.ts`, since its npm
  wrapper has no licence), `tauri-plugin-dialog` (folder picker), `tauri-plugin-autostart` (applied only
  when `app.autostart` changes, so the OS's own startup-apps choice isn't overridden at
  every start).
- The Library is the start page (M11.2; Home was folded into it, the user's choice).
  The recorder (`src/library/RecorderBar.svelte`: state, game, buffer meter counting
  between the 2 s polls, problem badge, Record / Save, Bookmark and the other lengths on
  wide windows; the ⓘ pop-up has every action, the recorder's details, problems with a
  link to Settings → Hotkeys, and the activity log) sits in the app bar, which is the
  window's title bar on Windows (M11.3, ADR 0014): `decorations(false)` there, the app
  draws minimize / maximize / close (`src/lib/WindowButtons.svelte`) and a press on a
  non-control part of the bar moves the window (`src/lib/window-chrome.ts`; double press
  maximizes). The mock shows the buttons with `?chrome`. Storage sits in the Details
  panel when nothing is selected; the empty grid names the first clip hotkey. No live
  capture preview (it would cost the game frames). The dot pulses a ring while a recording
  runs and blinks while the recorder starts (transform and opacity only).
- `src/lib/Panel.svelte` (panel with a tab strip), `src/App.svelte` (header, status bar).
- Icons: `src-tauri/icons/app-icon.svg` (made for Scout), sizes generated with
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
  Track locks (M9.8.10): every other track header has a lock too (`Track.locked`, saved
  with the project, an undoable edit). `trackLocked(t, ed.mainLocked)` says whether a
  track is locked; `selectClips` drops a locked track's clips (so Delete, `[`/`]`, S,
  Ctrl+K on a selection and the effect panels can't reach them), `unlockedClipIds` feeds
  Ctrl+A, Ctrl+K on every track and the Shift razor; a locked track takes no drops,
  moves, placements (`addClip`, `append_clips`) or transition edits, its lane is hatched,
  and an undo that locks a track again lets go of its clips. Its eye and M are greyed
  while locked. V1/A1 (M9.8.11) get V1's eye and A1's M (`setMainSwitches`: the main
  track's own `hidden`/`muted` when arranged, else `advanced.main_hidden`/`main_muted`;
  they move across `arrangeMain` / `useSimpleCut`), greyed while V1 is locked.
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

Release (ADR 0022): `cargo xtask bundle --sign [--notes <file>]` builds the installer
signed for the updater (`TAURI_SIGNING_PRIVATE_KEY` = the minisign private key or the
path of its file, `~/.tauri/scout.key` on the maintainer's PC; its public half is
`plugins.updater.pubkey` in `tauri.conf.json`) and writes `latest.json` beside it.
The installer carries Scout's icon, a header badge and a welcome/finish sidebar in the Dark
theme's colours (`bundle.windows.nsis` in `tauri.conf.json`; `tools/nsis-art.ps1` draws
the bitmaps into `src-tauri/icons/`), installs per user (no UAC) and offers "Run Scout"
at the end.
Pushing a tag `v<version>` that matches `Cargo.toml` runs
`.github/workflows/release.yml`, which does the same on a Windows runner (secrets
`TAURI_SIGNING_PRIVATE_KEY`, and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if the key has
one) and attaches the installer, its `.sig` and `latest.json` to a draft release;
publishing the draft is what makes the update check see it. Keep the key: a new one
means a manual reinstall for everyone.

Development and test overrides (environment): `KLIP_SETTINGS` (settings file),
`KLIP_DISCOVERY` (control-channel discovery file), `KLIP_START_PAGE=library|editor|settings`
(the page the window opens on; a Scout on its own `KLIP_SETTINGS` skips the
single-instance hand-over, so it runs beside the user's Scout instead of showing it; give
it its own `WEBVIEW2_USER_DATA_FOLDER` too), `KLIP_WINDOW_POS=x,y` (create the
window there, visible and **not activated**: tests put it off-screen, e.g. `12000,200`,
and capture it with `tools/window-shot.ps1`), `KLIP_UPDATE_URL` (where the update check
reads its manifest: a test server, or `off` for no check at all). Never
run a dev build with the real settings while someone plays: it would buffer their game
and take their hotkeys.

## Tests

- vitest: `src/lib/format.test.ts` (display helpers, which events show in Activity);
  `src/lib/appearance.test.ts` (M10): settings → look (custom theme over its base,
  missing theme → System, the editor's draft, text size / density / motion), every accent
  swatch keeps AA on all four themes, a custom theme's accent shades follow its accent,
  the contrast warning (built-ins clean, named pairs, translucent surfaces over a
  picture's darkest/lightest pixels), the CSS it writes (translucent surfaces only, URL
  and font escaping), `DEFAULT_FONT` = `app.css`, token names = the engine's.
  `src/lib/themes.test.ts`: every text/background pair the UI uses reads at WCAG AA
  (4.5:1) and every outline/focus/state colour stands out at 3:1, in all four themes
  (88 checks; it caught Light's warning orange on the status bar at 4.3:1); `app.css`
  matches Dark; setting → theme mapping.
- vitest `src/lib/library-actions.test.ts`: `setStar` (one failure doesn't stop the rest),
  `isFresh` (slashes and case, the 30 s window), `setWater`.
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
- e2e `src-tauri/tests/linux_start.rs` (Linux): the app starts on its own settings,
  stays up, and shows the overlay twice (on the desktop for a few seconds; Wayland lets
  no app place itself off-screen). Failed before the two Linux fixes above (abort at
  start-up, then exit on the first overlay).
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
