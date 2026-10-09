//! Win32/DXGI/WASAPI queries. Every `unsafe` block is a call into a documented OS API
//! with arguments owned by this function; nothing here dereferences foreign memory. All
//! of it is read-only: nothing touches other processes.

#![allow(unsafe_code)]

use std::ffi::c_void;

use std::collections::HashMap;

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND, LPARAM, POINT, RECT, S_OK};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIDevice,
    IDXGIFactory1, IDXGIOutput6,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW,
    GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW,
    MonitorFromWindow,
};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, AudioSessionStateExpired, DEVICE_STATE_ACTIVE, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoIncrementMTAUsage, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, MONITORINFOF_PRIMARY,
};
use windows::core::{BOOL, Interface, PCWSTR, PWSTR};

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
    let mut out: Vec<GpuAdapter> = Vec::new();
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
        // The same adapter can be enumerated more than once (e.g. when virtual display
        // drivers are installed); the LUID identifies it.
        if out.iter().any(|g| g.luid == luid) {
            continue;
        }
        // SAFETY: live interface; the IID is a static GUID.
        let driver_version = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }
            .ok()
            .and_then(|v| u64::try_from(v).ok());
        out.push(GpuAdapter {
            driver_version,
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
    let mut origin = POINT::default();
    // SAFETY: `origin` is a valid in/out pointer; on failure it stays (0, 0).
    let _ = unsafe { ClientToScreen(hwnd, &raw mut origin) };
    // SAFETY: MonitorFromWindow accepts any window handle; NEAREST always returns one.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    Ok(WindowInfo {
        handle,
        pid,
        title: String::from_utf16_lossy(title.get(..n).unwrap_or(&[])),
        client_width,
        client_height,
        client_x: origin.x,
        client_y: origin.y,
        minimized,
        monitor: monitor.0 as usize as u64,
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
    let outputs = dxgi_outputs().unwrap_or_default();
    let white = sdr_white_levels().unwrap_or_default();
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
        let handle = hmon.0 as usize as u64;
        let device_name = wide_to_string(&info.szDevice);
        let output = outputs.get(&handle);
        let mut mode = DEVMODEW {
            dmSize: u16::try_from(size_of::<DEVMODEW>()).unwrap_or(0),
            ..Default::default()
        };
        // SAFETY: `szDevice` is a NUL-terminated device name owned by `info`; `mode` is a
        // DEVMODEW with dmSize set.
        let refresh = unsafe {
            EnumDisplaySettingsW(
                PCWSTR(info.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &raw mut mode,
            )
        }
        .as_bool()
        // 0 and 1 mean "the hardware default".
        .then_some(mode.dmDisplayFrequency)
        .filter(|hz| *hz > 1);
        out.push(MonitorInfo {
            handle,
            x: r.left,
            y: r.top,
            width,
            height,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            adapter_luid: output.map(|o| o.luid),
            hdr: output.is_some_and(|o| o.hdr),
            sdr_white_nits: white.get(&device_name).copied(),
            max_nits: output.and_then(|o| (o.max_nits > 0.0).then_some(o.max_nits)),
            refresh_hz: refresh,
            device_name,
        });
    }
    Ok(out)
}

/// What DXGI knows about the output showing a monitor.
struct OutputInfo {
    luid: i64,
    hdr: bool,
    max_nits: f32,
}

/// HMONITOR → adapter LUID and HDR state, from every adapter's outputs.
fn dxgi_outputs() -> Result<HashMap<u64, OutputInfo>, PlatformError> {
    // SAFETY: CreateDXGIFactory1 has no preconditions; the returned COM reference is
    // released on drop.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(os)?;
    let mut map = HashMap::new();
    for a in 0u32..64 {
        // SAFETY: any index is valid; past the end it returns DXGI_ERROR_NOT_FOUND.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else {
            break;
        };
        // SAFETY: live interface.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let luid = i64::from(desc.AdapterLuid.HighPart).wrapping_shl(32)
            | i64::from(desc.AdapterLuid.LowPart);
        for o in 0u32..64 {
            // SAFETY: any index is valid; past the end it returns DXGI_ERROR_NOT_FOUND.
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else {
                break;
            };
            // IDXGIOutput6 exists on Windows 10 1803+; without it there is no HDR.
            let Ok(output6) = output.cast::<IDXGIOutput6>() else {
                // SAFETY: live interface.
                if let Ok(d) = unsafe { output.GetDesc() } {
                    map.entry(d.Monitor.0 as usize as u64)
                        .or_insert(OutputInfo {
                            luid,
                            hdr: false,
                            max_nits: 0.0,
                        });
                }
                continue;
            };
            // SAFETY: live interface.
            let Ok(d) = (unsafe { output6.GetDesc1() }) else {
                continue;
            };
            // The first adapter that lists a monitor drives it.
            map.entry(d.Monitor.0 as usize as u64)
                .or_insert(OutputInfo {
                    luid,
                    hdr: d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
                    max_nits: if d.MaxLuminance.is_finite() {
                        d.MaxLuminance
                    } else {
                        0.0
                    },
                });
        }
    }
    Ok(map)
}

/// GDI device name (`\\.\DISPLAY1`) → SDR white level in nits, from the display
/// configuration.
fn sdr_white_levels() -> Result<HashMap<String, f32>, PlatformError> {
    let mut n_paths = 0u32;
    let mut n_modes = 0u32;
    // SAFETY: valid flags and out pointers.
    let r = unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &raw mut n_paths, &raw mut n_modes)
    };
    if r != ERROR_SUCCESS {
        return Err(PlatformError::Os(format!(
            "GetDisplayConfigBufferSizes: {r:?}"
        )));
    }
    // Capped: a machine has a handful of displays.
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths.min(256) as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes.min(512) as usize];
    let mut n_paths = u32::try_from(paths.len()).unwrap_or(0);
    let mut n_modes = u32::try_from(modes.len()).unwrap_or(0);
    // SAFETY: the arrays hold the number of elements passed in the counts; the OS writes
    // at most that many and updates the counts.
    let r = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &raw mut n_paths,
            paths.as_mut_ptr(),
            &raw mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(PlatformError::Os(format!("QueryDisplayConfig: {r:?}")));
    }
    let mut map = HashMap::new();
    for p in paths.iter().take(n_paths as usize) {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: u32::try_from(size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>()).unwrap_or(0),
                adapterId: p.sourceInfo.adapterId,
                id: p.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: the header's type and size describe `source`, which is writable.
        if unsafe { DisplayConfigGetDeviceInfo(&raw mut source.header) } != 0 {
            continue;
        }
        let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                size: u32::try_from(size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>()).unwrap_or(0),
                adapterId: p.targetInfo.adapterId,
                id: p.targetInfo.id,
            },
            SDRWhiteLevel: 0,
        };
        // SAFETY: as above, for `white`.
        if unsafe { DisplayConfigGetDeviceInfo(&raw mut white.header) } != 0 {
            continue;
        }
        // 1000 = 80 nits (scRGB 1.0).
        let nits = white.SDRWhiteLevel as f32 * 80.0 / 1000.0;
        if nits > 0.0 && nits.is_finite() {
            map.insert(wide_to_string(&source.viewGdiDeviceName), nits);
        }
    }
    Ok(map)
}

pub(crate) fn foreground_window() -> Option<u64> {
    // SAFETY: no preconditions; returns NULL when no window has focus.
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(h.0 as usize as u64)
}

pub(crate) fn exclusive_fullscreen_active() -> bool {
    // SAFETY: no preconditions; returns the state by value.
    unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN)
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

pub(crate) fn process_path(pid: u32) -> Result<std::path::PathBuf, PlatformError> {
    // SAFETY: plain call; the handle is closed below. PROCESS_QUERY_LIMITED_INFORMATION is
    // the smallest right (no memory access) and is granted even for protected processes.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(os)?;
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).unwrap_or(0);
    // SAFETY: `buf` is writable for `len` u16s; `len` is updated to the length written.
    let res = unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &raw mut len)
    };
    // SAFETY: `h` came from OpenProcess and is closed exactly once.
    let _ = unsafe { CloseHandle(h) };
    res.map_err(os)?;
    let n = usize::try_from(len).unwrap_or(0).min(buf.len());
    Ok(std::path::PathBuf::from(String::from_utf16_lossy(
        buf.get(..n).unwrap_or(&[]),
    )))
}

pub(crate) fn process_usage(pid: u32) -> Result<crate::ProcessUsage, PlatformError> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetProcessTimes;

    // SAFETY: plain call; the handle is closed below. Limited information is enough for
    // times and memory counters (Windows 8.1+).
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(os)?;
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: four valid FILETIME out pointers.
    let times = unsafe {
        GetProcessTimes(
            h,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    let mut mem = PROCESS_MEMORY_COUNTERS_EX {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `mem` is a PROCESS_MEMORY_COUNTERS_EX with `cb` set to its size, passed as
    // the base struct pointer the API takes.
    let memory = unsafe {
        GetProcessMemoryInfo(h, (&raw mut mem).cast::<PROCESS_MEMORY_COUNTERS>(), mem.cb)
    };
    // SAFETY: `h` came from OpenProcess and is closed exactly once.
    let _ = unsafe { CloseHandle(h) };
    times.map_err(os)?;
    memory.map_err(os)?;
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Ok(crate::ProcessUsage {
        // FILETIME counts 100 ns.
        cpu_ns: ticks(kernel)
            .saturating_add(ticks(user))
            .saturating_mul(100),
        resident_bytes: u64::try_from(mem.WorkingSetSize).unwrap_or(u64::MAX),
        private_bytes: u64::try_from(mem.PrivateUsage).unwrap_or(u64::MAX),
    })
}

fn hwnd_of(handle: u64) -> Result<HWND, PlatformError> {
    let ptr = usize::try_from(handle).map_err(|_| PlatformError::NoSuchWindow(handle))?;
    let hwnd = HWND(ptr as *mut c_void);
    // SAFETY: IsWindow accepts any value and returns FALSE for invalid handles.
    if hwnd.0.is_null() || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(PlatformError::NoSuchWindow(handle));
    }
    Ok(hwnd)
}

/// Overlay style: never activated (by showing or clicking), no taskbar button, not in
/// Alt+Tab.
pub(crate) fn make_overlay(handle: u64) -> Result<(), PlatformError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };
    let hwnd = hwnd_of(handle)?;
    // SAFETY: valid window (checked above); reading and writing its extended style.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let add = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize;
    let remove = WS_EX_APPWINDOW.0 as isize;
    // SAFETY: as above.
    unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | add) & !remove) };
    Ok(())
}

/// Show above all other windows without activating it or moving the focus.
pub(crate) fn show_no_activate(handle: u64) -> Result<(), PlatformError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos,
    };
    let hwnd = hwnd_of(handle)?;
    // SAFETY: valid window; position and size are kept (NOMOVE | NOSIZE).
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .map_err(os)
}

pub(crate) fn hide_window(handle: u64) -> Result<(), PlatformError> {
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    let hwnd = hwnd_of(handle)?;
    // SAFETY: valid window.
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    Ok(())
}

pub(crate) fn focus_window(handle: u64) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    let Ok(hwnd) = hwnd_of(handle) else {
        return false;
    };
    // SAFETY: valid window; Windows only grants this to the foreground process.
    unsafe { SetForegroundWindow(hwnd) }.as_bool()
}

pub(crate) fn window_visible(handle: u64) -> bool {
    hwnd_of(handle)
        // SAFETY: valid window.
        .map(|h| unsafe { IsWindowVisible(h) }.as_bool())
        .unwrap_or(false)
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

/// Keep the process's multithreaded COM apartment alive for good, so WinRT objects
/// created on any thread (capture items, frame pools) stay usable whatever that thread
/// does with COM later. Idempotent.
pub(crate) fn com_init_mta() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: no preconditions; the cookie is deliberately never released.
        let _ = unsafe { CoIncrementMTAUsage() };
    });
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

/// Bytes available to this user on the volume holding `path`.
pub fn free_disk_space(path: &std::path::Path) -> Result<u64, PlatformError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::PCWSTR;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call; the out
    // pointer refers to a local u64; the other outputs are optional and passed as None.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut available), None, None) }
        .map_err(|e| PlatformError::Os(format!("GetDiskFreeSpaceExW: {e}")))?;
    Ok(available)
}
