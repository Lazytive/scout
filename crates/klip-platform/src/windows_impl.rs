//! Win32/DXGI queries. Every `unsafe` block is a call into a documented OS API with
//! arguments owned by this function; nothing here dereferences foreign memory.

#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIFactory1,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, MONITORINFOF_PRIMARY,
};
use windows::core::BOOL;

use crate::{GpuAdapter, GpuVendor, MonitorInfo, PlatformError, WindowInfo};

fn os(e: windows::core::Error) -> PlatformError {
    PlatformError::Os(e.message())
}

fn wide_to_string(w: &[u16]) -> String {
    let len = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(w.get(..len).unwrap_or(&[]))
}

fn rect_size(r: &RECT) -> (u32, u32) {
    let w = r.right.checked_sub(r.left).unwrap_or(0);
    let h = r.bottom.checked_sub(r.top).unwrap_or(0);
    (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0))
}

pub(crate) fn list_gpus() -> Result<Vec<GpuAdapter>, PlatformError> {
    // SAFETY: CreateDXGIFactory1 has no preconditions; the windows crate owns the
    // returned COM reference and releases it on drop.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(os)?;
    let mut out = Vec::new();
    for index in 0u32..64 {
        // SAFETY: any index is valid input; past the end it returns DXGI_ERROR_NOT_FOUND.
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(os(e)),
        };
        // SAFETY: `adapter` is a live interface returned by EnumAdapters1.
        let desc = unsafe { adapter.GetDesc1() }.map_err(os)?;
        let luid = i64::from(desc.AdapterLuid.HighPart).wrapping_shl(32)
            | i64::from(desc.AdapterLuid.LowPart);
        out.push(GpuAdapter {
            index,
            name: wide_to_string(&desc.Description),
            vendor: GpuVendor::from_pci_id(desc.VendorId),
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
            luid,
            dedicated_video_memory: u64::try_from(desc.DedicatedVideoMemory).unwrap_or(0),
            software: desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0,
        });
    }
    Ok(out)
}

unsafe extern "system" fn collect_hwnd(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the address of the `Vec<HWND>` that `list_windows` passes to
    // EnumWindows; it outlives the (synchronous) enumeration and nothing else aliases it.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    list.push(hwnd);
    BOOL(1)
}

pub(crate) fn list_windows() -> Result<Vec<WindowInfo>, PlatformError> {
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which lives until this returns.
    unsafe { EnumWindows(Some(collect_hwnd), LPARAM(&raw mut handles as isize)) }.map_err(os)?;
    let mut out = Vec::new();
    for hwnd in handles {
        // SAFETY: IsWindowVisible accepts any handle value.
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            continue;
        }
        if let Ok(info) = info_for(hwnd) {
            out.push(info);
        }
    }
    Ok(out)
}

fn info_for(hwnd: HWND) -> Result<WindowInfo, PlatformError> {
    let handle = hwnd.0 as usize as u64;
    // SAFETY: IsWindow accepts any handle value and returns FALSE for invalid ones.
    if hwnd.0.is_null() || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(PlatformError::NoSuchWindow(handle));
    }
    let mut title = [0u16; 512];
    // SAFETY: the buffer is a valid, writable slice; the call writes at most its length.
    let n = unsafe { GetWindowTextW(hwnd, &mut title) };
    let n = usize::try_from(n).unwrap_or(0).min(title.len());
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out pointer for the duration of the call.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out pointer; failure is reported as an error.
    unsafe { GetClientRect(hwnd, &raw mut rect) }.map_err(os)?;
    let (client_width, client_height) = rect_size(&rect);
    // SAFETY: IsIconic accepts any handle value.
    let minimized = unsafe { IsIconic(hwnd) }.as_bool();
    Ok(WindowInfo {
        handle,
        pid,
        title: String::from_utf16_lossy(title.get(..n).unwrap_or(&[])),
        client_width,
        client_height,
        minimized,
    })
}

pub(crate) fn window_info(handle: u64) -> Result<WindowInfo, PlatformError> {
    let ptr = usize::try_from(handle).map_err(|_| PlatformError::NoSuchWindow(handle))?;
    info_for(HWND(ptr as *mut c_void))
}

unsafe extern "system" fn collect_monitor(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: `lparam` is the address of the `Vec<HMONITOR>` owned by `list_monitors`,
    // alive for the synchronous enumeration.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HMONITOR>) };
    list.push(hmon);
    BOOL(1)
}

pub(crate) fn list_monitors() -> Result<Vec<MonitorInfo>, PlatformError> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which lives until this returns.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&raw mut handles as isize),
        )
    };
    if !ok.as_bool() {
        return Err(PlatformError::Os("EnumDisplayMonitors failed".into()));
    }
    let mut out = Vec::new();
    for hmon in handles {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = u32::try_from(size_of::<MONITORINFOEXW>()).unwrap_or(0);
        // SAFETY: `info` is a MONITORINFOEXW with cbSize set, so the OS may write the
        // extended structure through the MONITORINFO pointer.
        let ok = unsafe { GetMonitorInfoW(hmon, (&raw mut info).cast::<MONITORINFO>()) };
        if !ok.as_bool() {
            continue;
        }
        let r = info.monitorInfo.rcMonitor;
        let (width, height) = rect_size(&r);
        out.push(MonitorInfo {
            handle: hmon.0 as usize as u64,
            x: r.left,
            y: r.top,
            width,
            height,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            device_name: wide_to_string(&info.szDevice),
        });
    }
    Ok(out)
}

pub(crate) fn enable_dpi_awareness() -> bool {
    // SAFETY: no preconditions; fails harmlessly if awareness was already set.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_ok()
}
