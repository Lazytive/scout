# klip-platform

OS integration and the **only** crate allowed `unsafe` (AGENTS.md rule 3; enforced by
`cargo xtask layers`). Each `unsafe` block has a `// SAFETY:` comment.

Windows (Win32/DXGI via the `windows` crate):
- `list_gpus`: DXGI adapters (name, PCI vendor/device, LUID, VRAM, software flag, driver
  version), deduplicated by LUID. Virtual display drivers (Parsec, Virtual Desktop) make
  DXGI list the real GPU again under a *different* LUID; that copy has no outputs.
- `find_windows` / `window_info`: visible top-level windows, pid, client size and screen
  position, minimised, the monitor it is on. `WindowInfo::covers(monitor)` tells
  borderless/exclusive fullscreen. Titles are personal data: callers must not log them.
- `list_monitors`: HMONITOR, position/size, primary flag, the adapter LUID that drives it
  (DXGI outputs), Windows HDR on/off (`IDXGIOutput6` colour space), SDR white level
  (DisplayConfig, "SDR content brightness") and peak luminance.
- `exclusive_fullscreen_active`: the shell's "D3D full screen" notification state.
- `enable_dpi_awareness`: per-monitor-v2, so sizes are physical pixels.
- `move_to_trash(paths)` (both OSes): the `trash` crate (MIT) on a `klip-trash` thread
  of its own (its Windows code sets up COM for the calling thread). Diagnostic
  `diag_trash_moves_a_file` (manual: it leaves a file in the Recycle Bin).
- `copy_files(paths)` (M11.4, `clipboard.rs`, ADR 0021): "copied files" on the clipboard
  through `arboard`'s file list (Windows CF_HDROP, Linux `text/uri-list` on Wayland
  data-control or X11); one clipboard handle lives for the process (on Linux the
  program that set the clipboard serves it). `copied_files`, `clipboard_text`,
  `set_clipboard_text` for tests; `copied_files_read_back` (e2e) puts the text back.
- `open_file(path)` / `reveal_file(path)` (M7, `shell.rs`): the library's Open and Show
  in folder. Windows: `ShellExecuteW("open")` and `SHParseDisplayName` +
  `SHOpenFolderAndSelectItems` on a short-lived single-threaded-apartment thread. Linux:
  `xdg-open`; the freedesktop `FileManager1.ShowItems` through `gdbus`, else the folder
  with `xdg-open` (not yet run).
- `process_usage(pid)`: CPU time and memory (Windows: GetProcessTimes +
  GetProcessMemoryInfo; Linux: `/proc/<pid>/stat` + `statm`), for Scout's own processes
  (idle cost, benchmarks). Monitors report `refresh_hz` (EnumDisplaySettingsW).
- `list_processes`: pid, parent pid, executable name (Toolhelp snapshot).
  `process_path(pid)`: full executable path (`QueryFullProcessImageNameW` with
  `PROCESS_QUERY_LIMITED_INFORMATION`: no memory access, allowed even for protected
  processes). `list_windows`: all visible top-level windows (game detection).
- Overlay windows: `make_overlay` (`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, no
  `WS_EX_APPWINDOW`), `show_no_activate` (`SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE |
  SWP_SHOWWINDOW)`), `hide_window`, `focus_window` (hand the focus back to the game),
  `window_visible`.
- `GlobalShortcuts` (`hotkeys_win.rs`, ADR 0014): `RegisterHotKey` on a `klip-hotkeys`
  thread with its own message loop (`MOD_NOREPEAT`); triggers in the shortcuts-spec
  format are mapped to virtual keys by `keymap.rs` (US-layout punctuation). A combination
  another app holds comes back as `BoundShortcut::error` ("already used by another app")
  while the others work; dropping unregisters. `press_keys(trigger)` types a trigger
  with `SendInput` (tests; modifiers reach the focused app, so tests press bare F24).
- `audio_sessions`: processes with an audio session on any active output device (WASAPI
  session manager; system sounds and expired sessions left out), and whether each is
  playing. Used to pick apps for audio capture; exe names are shown to the user only.
- `AudioCapture` (`wasapi.rs`): a WASAPI capture thread for an output's loopback, a
  process tree (process loopback, include or exclude; async activation with a COM
  completion handler) or an input device. Float at the requested rate (AUTOCONVERTPCM),
  event-driven, every packet stamped with WASAPI's QPC capture time. Silence is generated
  while a stream delivers nothing; a failed or lost device becomes silence + a notice and
  is retried every 2 s. Packets that overlap generated silence are trimmed.
- `GpuCapture` asks Windows for at most twice the recording's frame rate
  (`GraphicsCaptureSession::MinUpdateInterval`, Windows 11 24H2+; ignored before).
- `GpuCapture` (`wgc.rs`, ADR 0010): Scout's own Windows.Graphics.Capture source for a
  window (client area) or monitor. D3D11 device on a chosen adapter, shared with
  GStreamer (`gst_d3d11.rs`: `GstD3D11Device` wrap, `GstContext`, D3D11 buffer pool,
  device lock); per frame one shader pass (crop, scale, letterbox; HDR monitors: FP16
  capture, scene-peak measurement, tone-map to SDR) into a pooled texture handed to a
  `GpuFrameSink` with WGC's QPC capture time. Counters in `GpuCaptureStats`.
- `free_disk_space(path)`: bytes available to the user on that volume
  (`GetDiskFreeSpaceExW`; Linux `statvfs`), for the recording reserve.
- `qpc_now_ns` (the clock of those stamps) and `process_loopback_supported` (build 19041+,
  via `RtlGetVersion`).

Gotcha: the `windows` crate implements `Drop` for `PROPVARIANT` (it calls
`PropVariantClear`). The process-loopback activation PROPVARIANT points at a stack struct,
so it is wrapped in `ManuallyDrop`; otherwise the heap is corrupted (found by
`audio_capture_is_continuous_and_qpc_stamped`). Diagnostic: `tests/loopback_offset.rs`
(`diag_`, needs `KLIP_DIAG_PID`) compares process and system loopback timing.
Hotkeys (`hotkey_tests.rs`): every `klip-settings` key maps to a virtual key; a duplicate
combination, an unknown key and a missing trigger are reported per shortcut and dropping
unregisters; hardware (`e2e`): a pressed F24 reaches the callback, twice.

Linux (`linux_impl.rs`, `screencast.rs`; ADR 0012):
- `list_gpus`: DRM cards from sysfs (PCI vendor/device ids, driver, PCI slot packed into
  `luid`); names from NVIDIA's `/proc/driver/nvidia/gpus/*/information`, else hwdata's
  `pci.ids`; driver version for NVIDIA (`/sys/module/nvidia/version`), VRAM for amdgpu.
- `list_processes`: `/proc/<pid>/stat` (parent pid parsed after the last `)`) and the
  `exe` link's file name (else `comm`).
- `qpc_now_ns`: CLOCK_MONOTONIC (`rustix`), the clock of GStreamer's system clock and
  PipeWire's stream times.
- `ScreenCast`: an xdg-desktop-portal ScreenCast session (`ashpd`, `async-io` backend,
  blocked on with `pollster`): source types, cursor mode, persist mode "until revoked",
  restore token in and out; streams (PipeWire node, monitor/window, logical position and
  size) and the PipeWire remote fd. Dropping it closes the session. A cancelled dialog
  is `PlatformError::Cancelled`; portal replies are handled inside `catch_unwind`.
- `ScreenCast::is_closed`: a thread owning the portal session waits for the desktop's
  `Closed` signal (the user stopped sharing) or for the `ScreenCast` to be dropped (then
  it closes the session).
- `AudioCapture` (`pipewire_audio.rs`, ADR 0008): PipeWire capture on its own thread and
  main loop: output monitor, input device, or apps' playback streams linked by Scout
  (include a process tree / everything except it; process ids from the client's
  `pipewire.sec.pid`, set by the daemon). Stamped with the graph cycle time minus the
  stream delay (CLOCK_MONOTONIC); silence while nothing plays; reconnects every 2 s.
- `audio_sessions`: processes with a playback stream, active when one is running (two
  registry round trips; nodes bound for their state).
- `timeline.rs`: the silence/overlap timeline shared with WASAPI.
- `GlobalShortcuts` (`shortcuts.rs`): the GlobalShortcuts portal (KDE asks the user to
  confirm the keys once); activations go to a callback on the portal thread.
  `register_app_id(APP_ID)` names the host app to the portals (needed for shortcuts; the
  portals also key screen-cast restore tokens by it) and first writes a hidden desktop
  entry (`NoDisplay=true`, `Exec` = this executable) to `$XDG_DATA_HOME/applications`,
  because the portal only accepts ids with a desktop entry whose program exists.
  `APP_ID = io.github.lazytive.Scout` (ADR 0020; the hidden entry of the old
  `dev.klip.Klip` id is removed). The entry names Scout's icon, written beside it
  (`icons/hicolor/256x256/apps`), and `set_desktop_app_id` (GLib's program name, set
  before GTK starts) gives the app's windows that id on Wayland, so the taskbar shows
  Scout's icon instead of a generic one for `klip-app`.
- Overlay (`layer_shell.rs`): `overlay_layer_init(title)` makes the overlay window a
  layer-shell surface (overlay layer, no keyboard) and `overlay_layer_place` sizes it
  and anchors it to a corner (or none: centred) of the output the compositor picks (KDE:
  the active one), as Wayland lets no window place itself. gtk-layer-shell (LGPL-3.0+,
  `libgtk-layer-shell.so.0`, 0.6 or later) is loaded at run time with `libloading`, and
  the GTK calls are looked up in the process; without it, or on a compositor without
  the protocol (GNOME), both return `Ok(false)` and the overlay stays a normal window
  (KDE puts that in the middle). Both run on the GTK main thread only (checked).
- `process_path`: `/proc/<pid>/exe`, or the command line's program for Wine/Proton
  loaders (untested until the next Linux session).
- Monitors and windows return `Unsupported` (Wayland has no window list).

Only documented OS APIs are used (WGC is the OS capture API). Nothing here touches
another process's memory or rendering (rule 1).
