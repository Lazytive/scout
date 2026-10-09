//! OS integration: GPU adapters, top-level windows, monitors, DPI awareness.
//!
//! This is the only crate allowed to use `unsafe` (AGENTS.md rule 3). Every `unsafe`
//! block carries a `// SAFETY:` comment. Only documented OS query APIs are used; nothing
//! here touches another process's memory or rendering (rule 1).

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

#[cfg(windows)]
mod wasapi;
#[cfg(windows)]
mod windows_impl;

/// Errors from OS queries.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("not supported on this platform")]
    Unsupported,
    #[error("window {0:#x} does not exist")]
    NoSuchWindow(u64),
    #[error("OS call failed: {0}")]
    Os(String),
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
    pub minimized: bool,
}

/// A monitor in physical pixels (when DPI awareness is enabled).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    /// HMONITOR on Windows.
    pub handle: u64,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub device_name: String,
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
    /// QueryPerformanceCounter time of the first frame, in ns (see [`qpc_now_ns`]).
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
        #[cfg(not(windows))]
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
        #[cfg(windows)]
        self.handle.shutdown();
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Current QueryPerformanceCounter time in ns: the clock of [`AudioPacket::qpc_ns`], WGC
/// frame times and (on Windows) GStreamer's system clock.
pub fn qpc_now_ns() -> i64 {
    #[cfg(windows)]
    {
        wasapi::qpc_now_ns()
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Whether per-process audio capture is available (Windows 10 2004+).
pub fn process_loopback_supported() -> bool {
    #[cfg(windows)]
    {
        wasapi::process_loopback_supported()
    }
    #[cfg(not(windows))]
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
    #[cfg(not(windows))]
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
    #[cfg(not(windows))]
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
    #[cfg(not(windows))]
    {
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

    #[cfg(windows)]
    #[test]
    fn os_queries_do_not_fail() {
        enable_dpi_awareness();
        let gpus = list_gpus().unwrap();
        assert!(
            !gpus.is_empty(),
            "DXGI always lists at least the basic render driver"
        );
        let monitors = list_monitors().unwrap();
        // Headless CI may have none, but sizes must be sane when present.
        for m in monitors {
            assert!(m.width > 0 && m.height > 0);
        }
        let _ = find_windows("").unwrap();
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

    #[cfg(windows)]
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
