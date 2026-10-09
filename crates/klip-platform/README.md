# klip-platform

OS integration and the **only** crate allowed `unsafe` (AGENTS.md rule 3; enforced by
`cargo xtask layers`). Each `unsafe` block has a `// SAFETY:` comment.

Windows (Win32/DXGI via the `windows` crate):
- `list_gpus`: DXGI adapters (name, PCI vendor/device, LUID, VRAM, software flag).
- `find_windows` / `window_info`: visible top-level windows, pid, client size, minimised.
  Titles are personal data: callers must not log them.
- `list_monitors`: HMONITOR, position/size, primary flag.
- `enable_dpi_awareness`: per-monitor-v2, so sizes are physical pixels.

Linux: returns `PlatformError::Unsupported` until M4 (portal + PipeWire live elsewhere).

Only documented query APIs are used. Nothing here touches another process's memory or
rendering (rule 1).
