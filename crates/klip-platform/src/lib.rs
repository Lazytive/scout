//! OS integration: GPU adapters, top-level windows, monitors, DPI awareness.
//!
//! This is the only crate allowed to use `unsafe` (AGENTS.md rule 3). Every `unsafe`
//! block carries a `// SAFETY:` comment. Only documented OS query APIs are used; nothing
//! here touches another process's memory or rendering (rule 1).

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

#[cfg(windows)]
mod gst_d3d11;
#[cfg(all(test, windows))]
mod hotkey_tests;
#[cfg(windows)]
mod hotkeys_win;
#[cfg(any(windows, test))]
mod keymap;
#[cfg(target_os = "linux")]
mod linux_impl;
#[cfg(target_os = "linux")]
mod pipewire_audio;
mod screencast;
mod shell;
mod shortcuts;
#[cfg(any(windows, target_os = "linux"))]
mod timeline;
#[cfg(windows)]
mod wasapi;
#[cfg(windows)]
mod wgc;
#[cfg(windows)]
mod windows_impl;

pub use screencast::{ScreenCast, ScreenCastRequest, ScreenCastSource, ScreenCastStream};
pub use shell::{open_file, reveal_file};
pub use shortcuts::{APP_ID, BoundShortcut, GlobalShortcuts, ShortcutSpec, register_app_id};
#[cfg(windows)]
pub use wgc::{
    GpuCapture, GpuCaptureConfig, GpuCaptureStats, GpuCaptureTarget, GpuFrameSink, HdrDisplay,
};

/// Errors from OS queries.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("not supported on this platform")]
    Unsupported,
    #[error("window {0:#x} does not exist")]
    NoSuchWindow(u64),
    #[error("OS call failed: {0}")]
    Os(String),
    #[error("cancelled by the user")]
    Cancelled,
}

/// GPU vendor, from the PCI vendor id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    /// Microsoft Basic Render Driver (software).
    Microsoft,
    Other(u32),
}

impl GpuVendor {
    pub fn from_pci_id(id: u32) -> GpuVendor {
        match id {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            0x1414 => GpuVendor::Microsoft,
            other => GpuVendor::Other(other),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "NVIDIA",
            GpuVendor::Amd => "AMD",
            GpuVendor::Intel => "Intel",
            GpuVendor::Microsoft => "Microsoft",
            GpuVendor::Other(_) => "other",
        }
    }
}

/// One GPU adapter as the OS enumerates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuAdapter {
    /// Enumeration index (DXGI adapter index on Windows).
    pub index: u32,
    pub name: String,
    pub vendor: GpuVendor,
    pub vendor_id: u32,
    pub device_id: u32,
    /// Locally unique id (DXGI LUID as a 64-bit value).
    pub luid: i64,
    pub dedicated_video_memory: u64,
    pub software: bool,
    /// User-mode driver version (DXGI `CheckInterfaceSupport`), changes with driver
    /// updates.
    pub driver_version: Option<u64>,
}

/// A top-level window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    /// HWND on Windows.
    pub handle: u64,
    pub pid: u32,
    /// Window title. Personal data: never log it.
    pub title: String,
    pub client_width: u32,
    pub client_height: u32,
    /// Top-left of the client area in screen coordinates (physical pixels when DPI
    /// awareness is enabled).
    pub client_x: i32,
    pub client_y: i32,
    pub minimized: bool,
    /// The monitor that has the largest part of the window (HMONITOR on Windows).
    pub monitor: u64,
}

impl WindowInfo {
    /// Whether the client area covers all of `m` (borderless or exclusive fullscreen).
    pub fn covers(&self, m: &MonitorInfo) -> bool {
        let right = i64::from(self.client_x).saturating_add(i64::from(self.client_width));
        let bottom = i64::from(self.client_y).saturating_add(i64::from(self.client_height));
        let m_right = i64::from(m.x).saturating_add(i64::from(m.width));
        let m_bottom = i64::from(m.y).saturating_add(i64::from(m.height));
        self.client_x <= m.x && self.client_y <= m.y && right >= m_right && bottom >= m_bottom
    }
}

/// A monitor in physical pixels (when DPI awareness is enabled).
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    /// HMONITOR on Windows.
    pub handle: u64,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub device_name: String,
    /// LUID of the GPU adapter that drives this monitor (`GpuAdapter::luid`), if DXGI
    /// lists it. On hybrid laptops the internal panel usually hangs off the iGPU.
    pub adapter_luid: Option<i64>,
    /// Windows HDR ("Use HDR") is on: the desktop is composed in scRGB and sent as
    /// BT.2020 PQ.
    pub hdr: bool,
    /// Brightness of SDR white on this monitor in nits (Windows "SDR content brightness"),
    /// when the OS reports it. 80 nits is scRGB 1.0.
    pub sdr_white_nits: Option<f32>,
    /// Peak luminance the display reports, in nits.
    pub max_nits: Option<f32>,
    /// Current refresh rate in Hz, when the OS reports one.
    pub refresh_hz: Option<u32>,
}

/// CPU time and memory of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessUsage {
    /// User + kernel CPU time since the process started, nanoseconds.
    pub cpu_ns: u64,
    /// Resident memory (Windows: working set), bytes.
    pub resident_bytes: u64,
    /// Memory only this process holds (Windows: private commit; Linux: resident minus
    /// shared), bytes.
    pub private_bytes: u64,
}

/// What an audio capture records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureEndpoint {
    /// Loopback of an output device (`None`: the default output when capture starts).
    Loopback { device: Option<String> },
    /// One process tree's audio (`include`), or everything except it (Windows 10 2004+).
    Process { pid: u32, include: bool },
    /// A capture device such as a microphone (`None`: the default input).
    Input { device: Option<String> },
}

/// A packet of captured audio: interleaved 32-bit float at the capture's rate/channels.
#[derive(Clone, Copy, Debug)]
pub struct AudioPacket<'a> {
    /// Performance-clock time of the first frame, in ns (see [`qpc_now_ns`]).
    pub qpc_ns: i64,
    pub frames: u32,
    /// `None` for silence.
    pub data: Option<&'a [f32]>,
    /// The source reported a gap before this packet.
    pub discontinuity: bool,
}

/// Receives captured audio on the capture thread. Must be quick.
pub trait AudioCaptureSink: Send {
    fn packet(&mut self, packet: AudioPacket<'_>);
    /// Problems (device unavailable, lost, back) as user-facing text.
    fn notice(&mut self, message: String);
}

/// A running audio capture (its own thread). Stops on drop.
pub struct AudioCapture {
    #[cfg(windows)]
    handle: wasapi::Handle,
    #[cfg(target_os = "linux")]
    handle: pipewire_audio::Handle,
}

impl std::fmt::Debug for AudioCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioCapture").finish_non_exhaustive()
    }
}

impl AudioCapture {
    /// Start capturing `endpoint` as float `rate` Hz x `channels`. A device that can't be
    /// opened (or is lost later) produces silence and a notice, and is retried.
    pub fn start(
        endpoint: CaptureEndpoint,
        rate: u32,
        channels: u16,
        sink: Box<dyn AudioCaptureSink>,
    ) -> Result<AudioCapture, PlatformError> {
        #[cfg(windows)]
        {
            Ok(AudioCapture {
                handle: wasapi::start(endpoint, rate, channels, sink)?,
            })
        }
        #[cfg(target_os = "linux")]
        {
            Ok(AudioCapture {
                handle: pipewire_audio::start(endpoint, rate, channels, sink)?,
            })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (endpoint, rate, channels, sink);
            Err(PlatformError::Unsupported)
        }
    }

    /// Stop and wait for the capture thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        #[cfg(any(windows, target_os = "linux"))]
        self.handle.shutdown();
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Current time of the OS performance clock in ns: the clock of [`AudioPacket::qpc_ns`],
/// captured frame times and GStreamer's system clock. Windows: QueryPerformanceCounter;
/// Linux: CLOCK_MONOTONIC (also PipeWire's clock).
pub fn qpc_now_ns() -> i64 {
    #[cfg(windows)]
    {
        wasapi::qpc_now_ns()
    }
    #[cfg(target_os = "linux")]
    {
        linux_impl::monotonic_ns()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        0
    }
}

/// Bytes free for this user on the volume holding `path` (which must exist).
pub fn free_disk_space(path: &std::path::Path) -> Result<u64, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::free_disk_space(path)
    }
    #[cfg(target_os = "linux")]
    {
        linux_impl::free_disk_space(path)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        Err(PlatformError::Unsupported)
    }
}

/// Whether per-process audio capture is available (Windows 10 2004+; Linux: PipeWire links
/// app streams).
pub fn process_loopback_supported() -> bool {
    #[cfg(windows)]
    {
        wasapi::process_loopback_supported()
    }
    #[cfg(target_os = "linux")]
    {
        true
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}

/// A running process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: u32,
    /// Executable file name, e.g. `Spotify.exe` (no path).
    pub exe: String,
}

/// A process with an audio session on an active output device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSession {
    pub pid: u32,
    /// Playing right now (otherwise the session exists but is idle).
    pub active: bool,
}

/// All running processes (pid, parent, executable name).
pub fn list_processes() -> Result<Vec<ProcessInfo>, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::list_processes()
    }
    #[cfg(target_os = "linux")]
    {
        linux_impl::list_processes()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(PlatformError::Unsupported)
    }
}

/// Processes that have an audio session on any active output device (system sounds
/// excluded), one entry per process.
pub fn audio_sessions() -> Result<Vec<AudioSession>, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::audio_sessions()
    }
    #[cfg(target_os = "linux")]
    {
        pipewire_audio::audio_sessions()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(PlatformError::Unsupported)
    }
}

/// Enumerate GPU adapters.
pub fn list_gpus() -> Result<Vec<GpuAdapter>, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::list_gpus()
    }
    #[cfg(target_os = "linux")]
    {
        linux_impl::list_gpus()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(PlatformError::Unsupported)
    }
}

/// All visible top-level windows (Windows; elsewhere unsupported: Wayland has no window
/// list).
pub fn list_windows() -> Result<Vec<WindowInfo>, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::list_windows()
    }
    #[cfg(not(windows))]
    {
        Err(PlatformError::Unsupported)
    }
}

/// Full path of a process's executable. Windows: `QueryFullProcessImageNameW` with
/// `PROCESS_QUERY_LIMITED_INFORMATION` (the least access there is; no memory access).
/// Linux: `/proc/<pid>/exe`, or for Wine/Proton loaders the program in the command line.
pub fn process_path(pid: u32) -> Result<std::path::PathBuf, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::process_path(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_impl::process_path(pid)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = pid;
        Err(PlatformError::Unsupported)
    }
}

/// CPU time and memory of a process (for Klip's own processes: idle cost, benchmarks).
pub fn process_usage(pid: u32) -> Result<ProcessUsage, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::process_usage(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_impl::process_usage(pid)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = pid;
        Err(PlatformError::Unsupported)
    }
}

/// Move files to the Recycle Bin (Windows) or the freedesktop Trash (Linux), one result
/// per file. Runs on a thread of its own: on Windows the `trash` crate initialises COM
/// (single-threaded apartment) for the calling thread, which must not clash with ours.
pub fn move_to_trash(paths: &[std::path::PathBuf]) -> Vec<Result<(), PlatformError>> {
    let owned = paths.to_vec();
    let n = owned.len();
    let job = std::thread::Builder::new()
        .name("klip-trash".into())
        .spawn(move || {
            owned
                .iter()
                .map(|p| {
                    trash::delete(p)
                        .map_err(|e| PlatformError::Os(format!("moving to the trash: {e}")))
                })
                .collect::<Vec<_>>()
        });
    let failed = |m: &str| (0..n).map(|_| Err(PlatformError::Os(m.into()))).collect();
    match job {
        Ok(h) => h
            .join()
            .unwrap_or_else(|_| failed("the trash thread panicked")),
        Err(_) => failed("no thread for the trash"),
    }
}

/// Make a window an overlay: it is never activated (not by showing it, not by clicks),
/// has no taskbar button and is not in Alt+Tab (Windows; elsewhere unsupported).
pub fn make_overlay(window: u64) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::make_overlay(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err(PlatformError::Unsupported)
    }
}

/// Show a window above all others without activating it (the game keeps the focus).
pub fn show_no_activate(window: u64) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::show_no_activate(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err(PlatformError::Unsupported)
    }
}

pub fn hide_window(window: u64) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::hide_window(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err(PlatformError::Unsupported)
    }
}

/// Give a window the keyboard focus (works only while our process has it, e.g. to hand
/// it back to the game after the quick menu).
pub fn focus_window(window: u64) -> bool {
    #[cfg(windows)]
    {
        windows_impl::focus_window(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        false
    }
}

/// Whether a window is shown.
pub fn window_visible(window: u64) -> bool {
    #[cfg(windows)]
    {
        windows_impl::window_visible(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        false
    }
}

/// Press and release a shortcut-spec trigger (e.g. `ALT+F10`) as if typed, system-wide.
/// For tests and automation (Windows; elsewhere unsupported). The modifiers reach the
/// focused app, so tests use bare keys no keyboard has (F13–F24).
pub fn press_keys(trigger: &str) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        hotkeys_win::press(trigger)
    }
    #[cfg(not(windows))]
    {
        let _ = trigger;
        Err(PlatformError::Unsupported)
    }
}

/// Visible top-level windows whose title contains `needle` (case-insensitive).
pub fn find_windows(needle: &str) -> Result<Vec<WindowInfo>, PlatformError> {
    #[cfg(windows)]
    {
        let needle = needle.to_lowercase();
        Ok(windows_impl::list_windows()?
            .into_iter()
            .filter(|w| w.title.to_lowercase().contains(&needle))
            .collect())
    }
    #[cfg(not(windows))]
    {
        let _ = needle;
        Err(PlatformError::Unsupported)
    }
}

/// Details of one window by handle.
pub fn window_info(handle: u64) -> Result<WindowInfo, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::window_info(handle)
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        Err(PlatformError::Unsupported)
    }
}

/// All monitors.
pub fn list_monitors() -> Result<Vec<MonitorInfo>, PlatformError> {
    #[cfg(windows)]
    {
        windows_impl::list_monitors()
    }
    #[cfg(not(windows))]
    {
        Err(PlatformError::Unsupported)
    }
}

/// The window that has the keyboard focus (HWND), if any.
pub fn foreground_window() -> Option<u64> {
    #[cfg(windows)]
    {
        windows_impl::foreground_window()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether the foreground app runs Direct3D exclusive fullscreen (the shell's
/// "running D3D full screen" notification state). Window capture can't see such a
/// game; capture its monitor instead.
pub fn exclusive_fullscreen_active() -> bool {
    #[cfg(windows)]
    {
        windows_impl::exclusive_fullscreen_active()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Ask the OS for per-monitor DPI awareness so window and monitor sizes are physical
/// pixels. Best effort: returns false if it was already set or isn't available.
pub fn enable_dpi_awareness() -> bool {
    #[cfg(windows)]
    {
        windows_impl::enable_dpi_awareness()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_ids() {
        assert_eq!(GpuVendor::from_pci_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(GpuVendor::from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(GpuVendor::from_pci_id(0x8086), GpuVendor::Intel);
        assert_eq!(GpuVendor::from_pci_id(0x1234), GpuVendor::Other(0x1234));
    }

    fn monitor(x: i32, y: i32, width: u32, height: u32) -> MonitorInfo {
        MonitorInfo {
            handle: 1,
            x,
            y,
            width,
            height,
            primary: false,
            device_name: String::new(),
            adapter_luid: None,
            hdr: false,
            sdr_white_nits: None,
            max_nits: None,
            refresh_hz: None,
        }
    }

    fn window(x: i32, y: i32, w: u32, h: u32) -> WindowInfo {
        WindowInfo {
            handle: 1,
            pid: 1,
            title: String::new(),
            client_width: w,
            client_height: h,
            client_x: x,
            client_y: y,
            minimized: false,
            monitor: 1,
        }
    }

    #[test]
    fn window_covers_monitor() {
        let m = monitor(2560, 0, 2560, 1440);
        assert!(window(2560, 0, 2560, 1440).covers(&m));
        assert!(window(2550, -10, 2600, 1500).covers(&m));
        assert!(!window(2560, 0, 2560, 1439).covers(&m));
        assert!(!window(0, 0, 2560, 1440).covers(&m));
        // Extreme values don't overflow.
        assert!(!window(i32::MAX, i32::MAX, u32::MAX, u32::MAX).covers(&m));
        assert!(window(i32::MIN, i32::MIN, u32::MAX, u32::MAX).covers(&m));
    }

    #[cfg(windows)]
    #[test]
    fn os_queries_do_not_fail() {
        enable_dpi_awareness();
        let gpus = list_gpus().unwrap();
        assert!(
            !gpus.is_empty(),
            "DXGI always lists at least the basic render driver"
        );
        for (i, g) in gpus.iter().enumerate() {
            assert!(
                gpus.iter().skip(i + 1).all(|o| o.luid != g.luid),
                "adapters are deduplicated by LUID"
            );
        }
        let monitors = list_monitors().unwrap();
        // Headless CI may have none, but values must be sane when present.
        for m in &monitors {
            assert!(m.width > 0 && m.height > 0);
            if let Some(l) = m.adapter_luid {
                assert!(gpus.iter().any(|g| g.luid == l), "{m:?}");
            }
            if let Some(n) = m.sdr_white_nits {
                assert!((40.0..=1000.0).contains(&n), "{m:?}");
            }
        }
        let _ = exclusive_fullscreen_active();
        for w in find_windows("").unwrap() {
            assert!(monitors.is_empty() || monitors.iter().any(|m| m.handle == w.monitor));
        }
        assert!(matches!(
            window_info(0),
            Err(PlatformError::NoSuchWindow(0))
        ));
        assert!(matches!(
            window_info(u64::MAX),
            Err(PlatformError::NoSuchWindow(_))
        ));
        let procs = list_processes().unwrap();
        let me = procs.iter().find(|p| p.pid == std::process::id()).unwrap();
        assert!(me.exe.to_lowercase().ends_with(".exe"), "{me:?}");
        assert!(procs.iter().any(|p| p.pid == me.parent_pid));
        // May be empty (no audio playing), but must not fail or list pid 0.
        assert!(audio_sessions().unwrap().iter().all(|s| s.pid != 0));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn audio_capture_is_continuous_and_qpc_stamped() {
        use std::sync::{Arc, Mutex};

        type Log = Arc<Mutex<Vec<(i64, u32)>>>;
        struct Collect(Log, Arc<Mutex<Vec<String>>>);
        impl AudioCaptureSink for Collect {
            fn packet(&mut self, p: AudioPacket<'_>) {
                if let Some(d) = p.data {
                    assert_eq!(d.len(), p.frames as usize * 2);
                }
                self.0.lock().unwrap().push((p.qpc_ns, p.frames));
            }
            fn notice(&mut self, message: String) {
                self.1.lock().unwrap().push(message);
            }
        }
        // Without an audio device, both still produce (generated) silence.
        for endpoint in [
            CaptureEndpoint::Process {
                pid: std::process::id(),
                include: true,
            },
            CaptureEndpoint::Loopback { device: None },
        ] {
            let log: Log = Arc::default();
            let notices = Arc::default();
            let t0 = qpc_now_ns();
            let cap = AudioCapture::start(
                endpoint.clone(),
                48_000,
                2,
                Box::new(Collect(log.clone(), Arc::clone(&notices))),
            )
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            cap.stop();
            let t1 = qpc_now_ns();
            let log = log.lock().unwrap();
            println!(
                "{endpoint:?}: {} packets, notices {:?}",
                log.len(),
                notices.lock().unwrap()
            );
            let frames: u64 = log.iter().map(|p| u64::from(p.1)).sum();
            assert!(
                frames >= 48_000 * 3 / 10,
                "{endpoint:?}: only {frames} frames"
            );
            for w in log.windows(2) {
                let end = w[0].0 + i64::from(w[0].1) * 1_000_000_000 / 48_000;
                assert!(
                    (w[1].0 - end).abs() < 2_000_000,
                    "{endpoint:?}: gap/overlap {w:?}"
                );
            }
            assert!(log[0].0 >= t0 - 100_000_000 && log.last().unwrap().0 <= t1);
        }
    }
}
