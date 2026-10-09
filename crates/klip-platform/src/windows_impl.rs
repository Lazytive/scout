//! Win32/DXGI/WASAPI queries. Every `unsafe` block is a call into a documented OS API
//! with arguments owned by this function; nothing here dereferences foreign memory. All
//! of it is read-only: nothing touches other processes.

#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, S_OK};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIFactory1,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, AudioSessionStateExpired, DEVICE_STATE_ACTIVE, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, MONITORINFOF_PRIMARY,
};
use windows::core::{BOOL, Interface};

use crate::{
    AudioSession, GpuAdapter, GpuVendor, MonitorInfo, PlatformError, ProcessInfo, WindowInfo,
};

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

pub(crate) fn list_processes() -> Result<Vec<ProcessInfo>, PlatformError> {
    // SAFETY: valid flags; the returned handle is closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(os)?;
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0),
        ..Default::default()
    };
    let mut out = Vec::new();
    // SAFETY: `snap` is a live snapshot handle and `entry` is a writable PROCESSENTRY32W
    // with dwSize set.
    let mut ok = unsafe { Process32FirstW(snap, &raw mut entry) }.is_ok();
    while ok {
        out.push(ProcessInfo {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            exe: wide_to_string(&entry.szExeFile),
        });
        // SAFETY: as above.
        ok = unsafe { Process32NextW(snap, &raw mut entry) }.is_ok();
    }
    // SAFETY: `snap` came from CreateToolhelp32Snapshot and is closed exactly once.
    let _ = unsafe { CloseHandle(snap) };
    Ok(out)
}

/// Uninitialises COM on drop if this thread's `CoInitializeEx` succeeded.
struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs with the successful CoInitializeEx in `com_init` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn com_init() -> ComGuard {
    // SAFETY: CoInitializeEx may be called on any thread. If the thread already uses another
    // apartment model it fails with RPC_E_CHANGED_MODE, COM is still usable, and we must not
    // uninitialise.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    ComGuard(hr.is_ok())
}

pub(crate) fn audio_sessions() -> Result<Vec<AudioSession>, PlatformError> {
    let _com = com_init();
    // SAFETY: standard in-process activation of the MMDevice enumerator.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(os)?;
    // SAFETY: live interface, valid flow and state mask.
    let devices =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }.map_err(os)?;
    // SAFETY: live interface.
    let count = unsafe { devices.GetCount() }.map_err(os)?;
    let mut out: Vec<AudioSession> = Vec::new();
    for i in 0..count {
        // SAFETY: `i` is below the collection's count.
        let Ok(device) = (unsafe { devices.Item(i) }) else {
            continue;
        };
        // SAFETY: live device; IAudioSessionManager2 is a documented activation target.
        let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) })
        else {
            continue;
        };
        // SAFETY: live interface.
        let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else {
            continue;
        };
        // SAFETY: live interface.
        let n = unsafe { sessions.GetCount() }.unwrap_or(0);
        for k in 0..n {
            // SAFETY: `k` is below the enumerator's count.
            let Ok(control) = (unsafe { sessions.GetSession(k) }) else {
                continue;
            };
            let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            // SAFETY: live interface.
            let state = unsafe { control.GetState() };
            // SAFETY: live interface.
            let system_sounds = unsafe { control.IsSystemSoundsSession() } == S_OK;
            // SAFETY: live interface.
            let pid = unsafe { control.GetProcessId() };
            let (Ok(state), Ok(pid)) = (state, pid) else {
                continue;
            };
            if pid == 0 || system_sounds || state == AudioSessionStateExpired {
                continue;
            }
            let active = state == AudioSessionStateActive;
            match out.iter_mut().find(|s| s.pid == pid) {
                Some(s) => s.active |= active,
                None => out.push(AudioSession { pid, active }),
            }
        }
    }
    Ok(out)
}
