//! OS integration: GPU adapters, top-level windows, monitors, DPI awareness.
//!
//! This is the only crate allowed to use `unsafe` (AGENTS.md rule 3). Every `unsafe`
//! block carries a `// SAFETY:` comment. Only documented OS query APIs are used; nothing
//! here touches another process's memory or rendering (rule 1).

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

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
    }
}
