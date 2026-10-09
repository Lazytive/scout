//! Klip's own Windows.Graphics.Capture source.
//!
//! Why not `d3d11screencapturesrc`: it stamps frames with its own clock slots, delivers
//! only 8-bit BGRA (HDR games come out clipped), and needs 8x polling to keep timing
//! error small (ADR 0010). Here a free-threaded WGC frame pool runs on a D3D11 device we
//! create on the encoder's GPU; every frame is drawn once (client-area crop, scale to the
//! output size, letterbox if the window's aspect changed) into a texture from a
//! GStreamer D3D11 buffer pool on the same device, and handed over with its capture time
//! on the QueryPerformanceCounter clock: when it reached us, bounded by WGC's
//! `SystemRelativeTime` (which is the later display time). The pipeline shares the device
//! through a `GstContext`, so frames never leave the GPU.
//!
//! Every `unsafe` block is a call into a documented D3D11/DXGI/WinRT/Win32 API with
//! arguments owned here; COM lifetimes are managed by the windows crate.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_video as gst_video;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND, LUID, POINT, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BIND_UNORDERED_ACCESS, D3D11_BUFFER_DESC, D3D11_BUFFER_UAV, D3D11_BUFFER_UAV_FLAG_RAW,
    D3D11_CPU_ACCESS_READ, D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE,
    D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_UAV_DIMENSION_BUFFER, D3D11_UNORDERED_ACCESS_VIEW_DESC,
    D3D11_UNORDERED_ACCESS_VIEW_DESC_0, D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC,
    D3D11_USAGE_STAGING, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11PixelShader, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11UnorderedAccessView,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_TYPELESS;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIFactory4,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, HMONITOR};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{IInspectable, Interface, PCSTR, s};

use crate::PlatformError;
use crate::gst_d3d11::{GstDevice, MappedTexture};

fn os(e: windows::core::Error) -> PlatformError {
    PlatformError::Os(e.message())
}

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuCaptureTarget {
    /// HWND; the client area is kept, the frame and title bar cut off.
    Window(u64),
    /// HMONITOR.
    Monitor(u64),
}

/// Settings for [`GpuCapture::start`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuCaptureConfig {
    pub target: GpuCaptureTarget,
    /// Create the capture device on this adapter (the encoder's), if given.
    pub adapter_luid: Option<i64>,
    pub show_cursor: bool,
    /// Size of the frames handed out (the picture is scaled to fit, keeping aspect).
    pub width: u32,
    pub height: u32,
    /// The target's monitor runs in Windows HDR mode: capture in FP16 (scRGB) and
    /// tone-map to SDR (see [`HdrDisplay`]). `None`: 8-bit capture, no conversion.
    pub hdr: Option<HdrDisplay>,
    /// The recording's frame rate (0: unknown). Windows is asked for at most twice as
    /// many frames (`MinUpdateInterval`, Windows 11 24H2+): frames arrive on composition
    /// ticks (e.g. 6.9 ms at 144 Hz), so a cap at exactly this rate would leave some
    /// recording slots empty; twice keeps every slot filled at 60–240 Hz and spares the
    /// desktop compositor the extra copies at high refresh rates.
    pub fps: u32,
}

/// What the tone mapping needs to know about an HDR display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrDisplay {
    /// Windows' SDR white level for the monitor ("SDR content brightness"); SDR content
    /// is composed at this brightness and comes out of the conversion unchanged.
    pub sdr_white_nits: f32,
    /// The display's peak; brighter content can't be seen on it, so highlights are
    /// rolled off up to here.
    pub peak_nits: f32,
}

impl Eq for HdrDisplay {}

/// Receives captured frames. Called on WGC's worker threads; must be quick.
pub trait GpuFrameSink: Send + Sync + 'static {
    /// A frame in D3D11 memory ([`GpuCapture::caps`]), captured at `qpc_ns`
    /// (QueryPerformanceCounter time in ns, the clock of WASAPI stamps).
    fn frame(&self, buffer: gst::Buffer, qpc_ns: i64);
    /// Capture stopped for good (window closed, device lost).
    fn ended(&self, reason: String);
}

/// Counters of a running capture.
#[derive(Debug, Default)]
pub struct GpuCaptureStats {
    /// Frames handed to the sink.
    pub frames: AtomicU64,
    /// Frames dropped because every output buffer was still in use downstream.
    pub dropped_busy: AtomicU64,
    /// Frames skipped (minimised window, zero-size content).
    pub skipped: AtomicU64,
    /// Frames that failed to draw.
    pub errors: AtomicU64,
    /// Times the source changed size.
    pub resizes: AtomicU64,
    /// HDR only: the brightest content seen recently, in nits (f32 bits).
    pub scene_peak_nits: AtomicU32,
    /// WGC frame time minus arrival time (µs): sum, count, extremes.
    pub lead_sum_us: AtomicI64,
    pub lead_count: AtomicU64,
    pub lead_min_us: AtomicI64,
    pub lead_max_us: AtomicI64,
}

/// One full-screen triangle. SDR: copy (crop/scale). HDR: scRGB in, sRGB out:
/// 1. scale so SDR white is 1.0 (SDR content is then exact), clip negative (out of
///    gamut) components;
/// 2. record the frame's peak (every 8th pixel in x and y, atomic max into `peak`);
/// 3. if the scene has content above SDR white, roll the brightest component off above a
///    knee in sRGB-encoded space, so the scene peak `tm.z` (encoded) lands on white;
///    colours keep their ratios;
/// 4. sRGB-encode.
const SHADER: &str = r"
cbuffer Params : register(b0) {
    float4 src_rect; // u0, v0, du, dv
    float4 tm;       // mode (0 copy, 1 HDR), 80 / SDR white, scene peak (encoded), knee
};
Texture2D src : register(t0);
SamplerState smp : register(s0);
RWByteAddressBuffer peak : register(u1);
struct VsOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VsOut vs_main(uint id : SV_VertexID) {
    VsOut o;
    float2 t = float2((id << 1) & 2, id & 2);
    o.pos = float4(t * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    o.uv = src_rect.xy + t * src_rect.zw;
    return o;
}
float enc(float v) { return v <= 0.0031308 ? 12.92 * v : 1.055 * pow(v, 1.0 / 2.4) - 0.055; }
float dec(float v) { return v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4); }
float4 ps_main(VsOut i) : SV_Target {
    float4 c = src.Sample(smp, i.uv);
    if (tm.x < 0.5) {
        return float4(c.rgb, 1.0);
    }
    float3 x = max(c.rgb * tm.y, 0.0);
    float m = max(max(x.r, x.g), x.b);
    uint2 p = uint2(i.pos.xy);
    if (((p.x | p.y) & 7u) == 0u) {
        peak.InterlockedMax(0, asuint(m));
    }
    float le = tm.z;
    float k = tm.w;
    if (le > 1.0001 && k < 0.9999 && m > 0.0) {
        float me = enc(m);
        if (me > k) {
            float e = me - k;
            float ye = k + e / (1.0 + e * (le - 1.0) / ((1.0 - k) * (le - k)));
            x *= dec(min(ye, 1.0)) / m;
        }
    }
    x = saturate(x);
    return float4(enc(x.r), enc(x.g), enc(x.b), 1.0);
}
";

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Params {
    src_rect: [f32; 4],
    params: [f32; 4],
}

/// GPU objects for the per-frame draw.
struct Render {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    cbuffer: ID3D11Buffer,
    /// Render target views of the pool's textures, by texture pointer.
    rtvs: Vec<(usize, ID3D11RenderTargetView)>,
    /// Size the WGC frame pool was created with.
    pool_size: SizeInt32,
    /// HDR: peak measurement (UAV) and its read-back ring.
    peak: Option<PeakMeter>,
}

/// GPU peak measurement for HDR: the shader atomically maxes into `buffer`; each frame
/// the result is copied to a staging buffer and read back two frames later (no stall).
struct PeakMeter {
    buffer: ID3D11Buffer,
    uav: ID3D11UnorderedAccessView,
    staging: [ID3D11Buffer; 3],
    frame: usize,
    /// Smoothed scene peak, SDR white = 1.0 (linear).
    level: f32,
    /// Highest useful level: display peak / SDR white.
    cap: f32,
    /// 80 / SDR white: scRGB → SDR white = 1.0.
    scale: f32,
}

/// sRGB encoding, extended above 1.0 with the same power law.
fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Tone-map parameters for a scene peak `level` (linear, SDR white = 1): (encoded peak,
/// knee). The knee moves from 1.0 (identity, no HDR content) to 0.8 once the peak is
/// 20 % (encoded) above SDR white.
fn tone_params(level: f32) -> (f32, f32) {
    let le = srgb_encode(level.max(1.0));
    let k = 1.0 - 0.2 * ((le - 1.0) / 0.2).clamp(0.0, 1.0);
    (le, k)
}

impl PeakMeter {
    fn new(device: &ID3D11Device, hdr: HdrDisplay) -> Result<PeakMeter, PlatformError> {
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
            MiscFlags: D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS.0 as u32,
            ..Default::default()
        };
        let staging_desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D11_UAV_DIMENSION_BUFFER,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D11_BUFFER_UAV {
                    FirstElement: 0,
                    NumElements: 4,
                    Flags: D3D11_BUFFER_UAV_FLAG_RAW.0 as u32,
                },
            },
        };
        let make = |d: &D3D11_BUFFER_DESC| -> Result<ID3D11Buffer, PlatformError> {
            let mut b = None;
            // SAFETY: valid descriptor and out pointer.
            unsafe { device.CreateBuffer(d, None, Some(&raw mut b)) }.map_err(os)?;
            b.ok_or_else(|| PlatformError::Os("CreateBuffer returned nothing".into()))
        };
        let buffer = make(&desc)?;
        let mut uav = None;
        // SAFETY: `buffer` is a raw-view UAV buffer of 16 bytes; the view covers 4 words.
        unsafe {
            device.CreateUnorderedAccessView(&buffer, Some(&raw const uav_desc), Some(&raw mut uav))
        }
        .map_err(os)?;
        let uav = uav.ok_or_else(|| PlatformError::Os("no UAV".into()))?;
        let staging = [
            make(&staging_desc)?,
            make(&staging_desc)?,
            make(&staging_desc)?,
        ];
        let white = hdr.sdr_white_nits.clamp(40.0, 1000.0);
        Ok(PeakMeter {
            buffer,
            uav,
            staging,
            frame: 0,
            level: 1.0,
            cap: (hdr.peak_nits / white).max(1.0),
            scale: 80.0 / white,
        })
    }

    /// Fold in the newest finished measurement (if the GPU has one ready).
    fn update(&mut self, ctx: &ID3D11DeviceContext) {
        let Some(old) = self.staging.get(self.frame.wrapping_add(1) % 3) else {
            return;
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `old` is our staging buffer; DO_NOT_WAIT returns an error instead of
        // blocking while the copy is in flight.
        let ok = unsafe {
            ctx.Map(
                old,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&raw mut mapped),
            )
        }
        .is_ok();
        if !ok || mapped.pData.is_null() {
            return;
        }
        // SAFETY: the staging buffer is 16 bytes and mapped for reading.
        let bits = unsafe { std::ptr::read_unaligned(mapped.pData.cast::<u32>()) };
        // SAFETY: pairs with the successful Map above.
        unsafe { ctx.Unmap(old, 0) };
        let m = f32::from_bits(bits);
        if !m.is_finite() || m < 0.0 {
            return;
        }
        let target = m.clamp(1.0, self.cap);
        // Rise within a few frames, fall over seconds (no pumping on short flashes).
        let rate = if target > self.level { 0.3 } else { 0.01 };
        self.level += (target - self.level) * rate;
    }
}

struct Shared {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    gst: GstDevice,
    winrt_device: IDirect3DDevice,
    out_pool: gst::BufferPool,
    out_size: (u32, u32),
    target: GpuCaptureTarget,
    format: DirectXPixelFormat,
    render: Mutex<Render>,
    sink: Arc<dyn GpuFrameSink>,
    stats: Arc<GpuCaptureStats>,
    stopped: AtomicBool,
}

// SAFETY: the D3D11 device is free-threaded and set multithread-protected; every use of
// the immediate context happens under GStreamer's device lock (`GstDevice::lock`) and our
// `render` mutex; the WinRT device wrapper is agile. Nothing here is tied to a thread.
unsafe impl Send for Shared {}
// SAFETY: as above; shared access goes through the locks.
unsafe impl Sync for Shared {}

/// A running capture. Stops on drop.
pub struct GpuCapture {
    shared: Arc<Shared>,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item: GraphicsCaptureItem,
    frame_token: i64,
    closed_token: i64,
    caps: gst::Caps,
}

impl std::fmt::Debug for GpuCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuCapture")
            .field("target", &self.shared.target)
            .field("out_size", &self.shared.out_size)
            .finish_non_exhaustive()
    }
}

/// D3D11 device on the adapter with `luid` (or the default adapter).
fn create_device(luid: Option<i64>) -> Result<(ID3D11Device, ID3D11DeviceContext), PlatformError> {
    // SAFETY: no preconditions; COM references are released on drop.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(os)?;
    let adapter: IDXGIAdapter1 = match luid {
        Some(l) => {
            let f4: IDXGIFactory4 = factory.cast().map_err(os)?;
            let luid = LUID {
                LowPart: (l & 0xFFFF_FFFF) as u32,
                HighPart: (l >> 32) as i32,
            };
            // SAFETY: plain value argument; returns a new reference or an error.
            unsafe { f4.EnumAdapterByLuid(luid) }.map_err(os)?
        }
        // SAFETY: index 0 always exists when any adapter does.
        None => unsafe { factory.EnumAdapters1(0) }.map_err(os)?,
    };
    let attempts: [(D3D11_CREATE_DEVICE_FLAG, &[_]); 3] = [
        (
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            &[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0],
        ),
        (
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            &[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0],
        ),
        (D3D11_CREATE_DEVICE_BGRA_SUPPORT, &[D3D_FEATURE_LEVEL_11_0]),
    ];
    let mut last = None;
    for (flags, levels) in attempts {
        let mut device = None;
        let mut context = None;
        // SAFETY: valid adapter, flags and out pointers; the driver type must be UNKNOWN
        // when an adapter is given.
        let r = unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                Some(levels),
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                Some(&raw mut context),
            )
        };
        match (r, device, context) {
            (Ok(()), Some(d), Some(c)) => {
                if let Ok(mt) = d.cast::<ID3D11Multithread>() {
                    // SAFETY: live interface; makes the immediate context thread-safe.
                    let _ = unsafe { mt.SetMultithreadProtected(true) };
                }
                return Ok((d, c));
            }
            (Err(e), _, _) => last = Some(e),
            _ => {}
        }
    }
    Err(last.map_or_else(|| PlatformError::Os("D3D11CreateDevice failed".into()), os))
}

fn compile(src: &str, entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, PlatformError> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: `src` outlives the call; entry/target are static NUL-terminated strings;
    // the out pointers are valid.
    let r = unsafe {
        D3DCompile(
            src.as_ptr().cast::<c_void>(),
            src.len(),
            PCSTR::null(),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &raw mut code,
            Some(&raw mut errors),
        )
    };
    let blob_bytes = |b: &ID3DBlob| -> Vec<u8> {
        // SAFETY: the blob owns `GetBufferSize()` bytes at `GetBufferPointer()` while it
        // lives; we copy them out.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
            .to_vec()
    };
    match (r, code) {
        (Ok(()), Some(c)) => Ok(blob_bytes(&c)),
        (r, _) => {
            let msg = errors
                .as_ref()
                .map(|e| String::from_utf8_lossy(&blob_bytes(e)).into_owned())
                .unwrap_or_else(|| format!("{r:?}"));
            Err(PlatformError::Os(format!("shader compile failed: {msg}")))
        }
    }
}

fn create_render(
    device: &ID3D11Device,
    pool_size: SizeInt32,
    hdr: Option<HdrDisplay>,
) -> Result<Render, PlatformError> {
    let vs_code = compile(SHADER, s!("vs_main"), s!("vs_5_0"))?;
    let ps_code = compile(SHADER, s!("ps_main"), s!("ps_5_0"))?;
    let mut vs = None;
    let mut ps = None;
    let mut sampler = None;
    let mut cbuffer = None;
    let sampler_desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MaxLOD: f32::MAX,
        ..Default::default()
    };
    let cb_desc = D3D11_BUFFER_DESC {
        ByteWidth: u32::try_from(size_of::<Params>()).unwrap_or(32),
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        ..Default::default()
    };
    // SAFETY: valid bytecode slices, descriptors and out pointers for each call.
    unsafe {
        device
            .CreateVertexShader(&vs_code, None, Some(&raw mut vs))
            .map_err(os)?;
        device
            .CreatePixelShader(&ps_code, None, Some(&raw mut ps))
            .map_err(os)?;
        device
            .CreateSamplerState(&raw const sampler_desc, Some(&raw mut sampler))
            .map_err(os)?;
        device
            .CreateBuffer(&raw const cb_desc, None, Some(&raw mut cbuffer))
            .map_err(os)?;
    }
    let peak = match hdr {
        Some(h) => Some(PeakMeter::new(device, h)?),
        None => None,
    };
    match (vs, ps, sampler, cbuffer) {
        (Some(vs), Some(ps), Some(sampler), Some(cbuffer)) => Ok(Render {
            vs,
            ps,
            sampler,
            cbuffer,
            rtvs: Vec::new(),
            pool_size,
            peak,
        }),
        _ => Err(PlatformError::Os(
            "D3D11 object creation returned nothing".into(),
        )),
    }
}

/// The client area of `hwnd` inside its WGC frame (which covers the window's visible
/// bounds), clamped to `content`. `None` while the window has no client area.
fn client_crop(hwnd: HWND, content: SizeInt32) -> Option<(u32, u32, u32, u32)> {
    let mut bounds = RECT::default();
    // SAFETY: `bounds` is a writable RECT of the size passed.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut bounds).cast::<c_void>(),
            u32::try_from(size_of::<RECT>()).ok()?,
        )
    }
    .ok()?;
    let mut origin = POINT::default();
    // SAFETY: valid in/out pointer.
    if !unsafe { ClientToScreen(hwnd, &raw mut origin) }.as_bool() {
        return None;
    }
    let mut client = RECT::default();
    // SAFETY: valid out pointer.
    unsafe { GetClientRect(hwnd, &raw mut client) }.ok()?;
    let cw = u32::try_from(content.Width).ok()?;
    let ch = u32::try_from(content.Height).ok()?;
    let x = u32::try_from(origin.x.checked_sub(bounds.left)?.max(0))
        .ok()?
        .min(cw);
    let y = u32::try_from(origin.y.checked_sub(bounds.top)?.max(0))
        .ok()?
        .min(ch);
    let w = u32::try_from(client.right.max(0))
        .ok()?
        .min(cw.saturating_sub(x));
    let h = u32::try_from(client.bottom.max(0))
        .ok()?
        .min(ch.saturating_sub(y));
    (w > 0 && h > 0).then_some((x, y, w, h))
}

/// Largest rectangle of the aspect `src_w:src_h` centred in `out_w`x`out_h`.
fn letterbox(src_w: u32, src_h: u32, out_w: u32, out_h: u32) -> (f32, f32, f32, f32) {
    let (sw, sh, ow, oh) = (src_w as f32, src_h as f32, out_w as f32, out_h as f32);
    if sw <= 0.0 || sh <= 0.0 {
        return (0.0, 0.0, ow, oh);
    }
    let scale = (ow / sw).min(oh / sh);
    let (w, h) = ((sw * scale).round(), (sh * scale).round());
    (((ow - w) / 2.0).floor(), ((oh - h) / 2.0).floor(), w, h)
}

impl Shared {
    fn on_frame(&self, pool: &Direct3D11CaptureFramePool) -> Result<(), String> {
        let arrival = crate::qpc_now_ns();
        let frame = pool.TryGetNextFrame().map_err(|e| e.message())?;
        let system = frame
            .SystemRelativeTime()
            .map_err(|e| e.message())?
            .Duration
            .saturating_mul(100);
        // WGC's frame time is about one display refresh *after* the frame reaches us
        // (measured +7.1 ms mean on a 120 Hz monitor: the vblank it will be shown at), so
        // it would put video late against audio. The frame's capture time is the
        // earlier of the two: arrival, bounded by WGC's time when our callback runs late.
        let lead_us = system.saturating_sub(arrival) / 1000;
        self.stats.lead_sum_us.fetch_add(lead_us, Ordering::Relaxed);
        self.stats.lead_count.fetch_add(1, Ordering::Relaxed);
        self.stats.lead_max_us.fetch_max(lead_us, Ordering::Relaxed);
        self.stats.lead_min_us.fetch_min(lead_us, Ordering::Relaxed);
        let qpc_ns = system.min(arrival);
        let content = frame.ContentSize().map_err(|e| e.message())?;
        let crop = match self.target {
            GpuCaptureTarget::Window(h) => client_crop(HWND(h as usize as *mut c_void), content),
            GpuCaptureTarget::Monitor(_) => {
                let w = u32::try_from(content.Width).ok().filter(|w| *w > 0);
                let h = u32::try_from(content.Height).ok().filter(|h| *h > 0);
                w.zip(h).map(|(w, h)| (0, 0, w, h))
            }
        };
        let resized = {
            let r = self.render.lock().unwrap_or_else(PoisonError::into_inner);
            r.pool_size.Width != content.Width || r.pool_size.Height != content.Height
        };
        let Some(crop) = crop else {
            self.stats.skipped.fetch_add(1, Ordering::Relaxed);
            drop(frame);
            if resized {
                self.recreate(pool, content)?;
            }
            return Ok(());
        };
        let surface = frame.Surface().map_err(|e| e.message())?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| e.message())?;
        // SAFETY: the surface wraps a D3D11 texture of our device.
        let src: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(|e| e.message())?;
        let acquire =
            gst::BufferPoolAcquireParams::with_flags(gst::BufferPoolAcquireFlags::DONTWAIT);
        let Ok(mut buffer) = self.out_pool.acquire_buffer(Some(&acquire)) else {
            self.stats.dropped_busy.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        };
        {
            let b = buffer.get_mut().ok_or("output buffer not writable")?;
            let mapped = MappedTexture::map(b).map_err(|e| e.to_string())?;
            self.draw(&src, content, crop, mapped.texture())?;
        }
        // Return WGC's texture before handing ours on.
        drop(src);
        let _ = frame.Close();
        if resized {
            self.recreate(pool, content)?;
        }
        self.stats.frames.fetch_add(1, Ordering::Relaxed);
        self.sink.frame(buffer, qpc_ns);
        Ok(())
    }

    fn recreate(&self, pool: &Direct3D11CaptureFramePool, size: SizeInt32) -> Result<(), String> {
        if size.Width <= 0 || size.Height <= 0 {
            return Ok(());
        }
        pool.Recreate(&self.winrt_device, self.format, 2, size)
            .map_err(|e| e.message())?;
        self.render
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pool_size = size;
        self.stats.resizes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Draw the `crop` of `src` (`content` big) into `dst`, scaled to fit.
    fn draw(
        &self,
        src: &ID3D11Texture2D,
        content: SizeInt32,
        crop: (u32, u32, u32, u32),
        dst: &ID3D11Texture2D,
    ) -> Result<(), String> {
        let mut r = self.render.lock().unwrap_or_else(PoisonError::into_inner);
        let key = dst.as_raw() as usize;
        let rtv = match r.rtvs.iter().find(|(k, _)| *k == key) {
            Some((_, v)) => v.clone(),
            None => {
                let mut v = None;
                // SAFETY: `dst` is a live render-target texture of our device.
                unsafe {
                    self.device
                        .CreateRenderTargetView(dst, None, Some(&raw mut v))
                }
                .map_err(|e| e.message())?;
                let v = v.ok_or("no render target view")?;
                if r.rtvs.len() >= 32 {
                    r.rtvs.clear();
                }
                r.rtvs.push((key, v.clone()));
                v
            }
        };
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: `src` is a live texture of our device with shader-resource binding.
        unsafe {
            self.device
                .CreateShaderResourceView(src, None, Some(&raw mut srv))
        }
        .map_err(|e| e.message())?;
        let cw = content.Width.max(1) as f32;
        let ch = content.Height.max(1) as f32;
        let tm = match &r.peak {
            Some(p) => {
                let (le, k) = tone_params(p.level);
                [1.0, p.scale, le, k]
            }
            None => [0.0; 4],
        };
        let params = Params {
            src_rect: [
                crop.0 as f32 / cw,
                crop.1 as f32 / ch,
                crop.2 as f32 / cw,
                crop.3 as f32 / ch,
            ],
            params: tm,
        };
        let (vx, vy, vw, vh) = letterbox(crop.2, crop.3, self.out_size.0, self.out_size.1);
        let viewport = D3D11_VIEWPORT {
            TopLeftX: vx,
            TopLeftY: vy,
            Width: vw,
            Height: vh,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let full = vw as u32 == self.out_size.0 && vh as u32 == self.out_size.1;
        let _lock = self.gst.lock();
        let ctx = &self.context;
        // SAFETY: all objects belong to our device and live through the calls; the
        // immediate context is used under the device lock. Every state the draw depends
        // on is set here, since GStreamer's elements share this context.
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(
                &r.cbuffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&raw mut mapped),
            )
            .map_err(|e| e.message())?;
            if mapped.pData.is_null() {
                ctx.Unmap(&r.cbuffer, 0);
                return Err("constant buffer map returned no data".into());
            }
            std::ptr::write_unaligned(mapped.pData.cast::<Params>(), params);
            ctx.Unmap(&r.cbuffer, 0);
            if !full {
                ctx.ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]);
            }
            match &r.peak {
                Some(p) => {
                    ctx.ClearUnorderedAccessViewUint(&p.uav, &[0; 4]);
                    let uavs = [Some(p.uav.clone())];
                    ctx.OMSetRenderTargetsAndUnorderedAccessViews(
                        Some(&[Some(rtv.clone())]),
                        None,
                        1,
                        1,
                        Some(uavs.as_ptr()),
                        None,
                    );
                }
                None => ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None),
            }
            ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
            ctx.OMSetDepthStencilState(None, 0);
            ctx.RSSetState(None);
            ctx.RSSetViewports(Some(&[viewport]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&r.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(r.cbuffer.clone())]));
            ctx.PSSetShader(&r.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(r.cbuffer.clone())]));
            ctx.PSSetShaderResources(0, Some(&[srv]));
            ctx.PSSetSamplers(0, Some(&[Some(r.sampler.clone())]));
            ctx.Draw(3, 0);
            // Unbind, so GStreamer can use the output as an input right away.
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
        if let Some(p) = r.peak.as_mut() {
            p.frame = p.frame.wrapping_add(1);
            if let Some(dst) = p.staging.get(p.frame % 3) {
                // SAFETY: both buffers are ours, same size; under the device lock.
                unsafe { ctx.CopyResource(dst, &p.buffer) };
            }
            p.update(ctx);
            let nits = p.level * 80.0 / p.scale;
            self.stats
                .scene_peak_nits
                .store(nits.to_bits(), Ordering::Relaxed);
        }
        Ok(())
    }
}

impl GpuCapture {
    /// Start capturing `cfg.target`. Frames go to `sink` until [`GpuCapture::stop`].
    pub fn start(
        cfg: &GpuCaptureConfig,
        sink: Arc<dyn GpuFrameSink>,
    ) -> Result<GpuCapture, PlatformError> {
        if cfg.width == 0 || cfg.height == 0 || cfg.width > 16384 || cfg.height > 16384 {
            return Err(PlatformError::Os(format!(
                "bad output size {}x{}",
                cfg.width, cfg.height
            )));
        }
        crate::windows_impl::com_init_mta();
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err(PlatformError::Unsupported);
        }
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(os)?;
        let item: GraphicsCaptureItem = match cfg.target {
            GpuCaptureTarget::Window(h) => {
                // SAFETY: any HWND value is accepted; invalid ones return an error.
                unsafe { interop.CreateForWindow(HWND(h as usize as *mut c_void)) }
            }
            GpuCaptureTarget::Monitor(h) => {
                // SAFETY: as above, for an HMONITOR.
                unsafe { interop.CreateForMonitor(HMONITOR(h as usize as *mut c_void)) }
            }
        }
        .map_err(os)?;
        let (device, context) = create_device(cfg.adapter_luid)?;
        let gst = GstDevice::wrap(&device)?;
        let dxgi: IDXGIDevice = device.cast().map_err(os)?;
        // SAFETY: `dxgi` is the live DXGI view of our device.
        let inspectable: IInspectable =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }.map_err(os)?;
        let winrt_device: IDirect3DDevice = inspectable.cast().map_err(os)?;
        let size = item.Size().map_err(os)?;
        let format = if cfg.hdr.is_some() {
            DirectXPixelFormat::R16G16B16A16Float
        } else {
            DirectXPixelFormat::B8G8R8A8UIntNormalized
        };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device, format, 2, size)
            .map_err(os)?;
        let (out_pool, caps) = gst.texture_pool(
            gst_video::VideoFormat::Bgra,
            cfg.width,
            cfg.height,
            4,
            12,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        )?;
        let render = create_render(&device, size, cfg.hdr)?;
        let stats = Arc::new(GpuCaptureStats {
            lead_min_us: AtomicI64::new(i64::MAX),
            lead_max_us: AtomicI64::new(i64::MIN),
            ..Default::default()
        });
        let shared = Arc::new(Shared {
            device,
            context,
            gst,
            winrt_device,
            out_pool,
            out_size: (cfg.width, cfg.height),
            target: cfg.target,
            format,
            render: Mutex::new(render),
            sink,
            stats,
            stopped: AtomicBool::new(false),
        });
        let s = shared.clone();
        let frame_token = pool
            .FrameArrived(&TypedEventHandler::new(
                move |sender: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
                    if s.stopped.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let Ok(pool) = sender.ok() else {
                        return Ok(());
                    };
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.on_frame(pool)));
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            let n = s.stats.errors.fetch_add(1, Ordering::Relaxed);
                            if n < 3 {
                                tracing::warn!(error = %e, "capture frame failed");
                            }
                        }
                        Err(_) => {
                            s.stats.errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(os)?;
        let s = shared.clone();
        let closed_token = item
            .Closed(&TypedEventHandler::new(move |_, _| {
                if !s.stopped.swap(true, Ordering::Relaxed) {
                    s.sink.ended("capture target closed".into());
                }
                Ok(())
            }))
            .map_err(os)?;
        let session = pool.CreateCaptureSession(&item).map_err(os)?;
        let _ = session.SetIsCursorCaptureEnabled(cfg.show_cursor);
        // Windows 11 only; the yellow border stays on Windows 10.
        let _ = session.SetIsBorderRequired(false);
        if let Some(ticks) = 10_000_000_i64.checked_div(i64::from(cfg.fps.saturating_mul(2))) {
            // Fewer frames than the desktop composes means less work for DWM and us.
            let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan { Duration: ticks });
        }
        session.StartCapture().map_err(os)?;
        Ok(GpuCapture {
            shared,
            pool,
            session,
            item,
            frame_token,
            closed_token,
            caps,
        })
    }

    /// Caps of the frames: `video/x-raw(memory:D3D11Memory)`, BGRA, the output size.
    pub fn caps(&self) -> &gst::Caps {
        &self.caps
    }

    /// Set this on the pipeline so its D3D11 elements use the capture device.
    pub fn context(&self) -> gst::Context {
        self.shared.gst.context()
    }

    pub fn stats(&self) -> Arc<GpuCaptureStats> {
        self.shared.stats.clone()
    }

    /// Stop capturing (also on drop).
    pub fn stop(self) {}

    fn shutdown(&mut self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
        let _ = self.shared.out_pool.set_active(false);
    }
}

impl Drop for GpuCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_curve_is_identity_for_sdr_and_rolls_off_hdr() {
        // No HDR content: knee at 1.0 (the shader skips the roll-off).
        for level in [0.5, 1.0] {
            let (le, k) = tone_params(level);
            assert!(
                (le - 1.0).abs() < 1e-5 && (k - 1.0).abs() < 1e-4,
                "{le} {k}"
            );
        }
        // Bright scene: knee at 0.8 (encoded), peak above 1.
        let (le, k) = tone_params(3.33);
        assert!((k - 0.8).abs() < 1e-6 && le > 1.5, "{le} {k}");
        // The curve the shader applies: monotonic, continuous at the knee, peak → 1.
        let curve = |me: f32| {
            if me <= k {
                me
            } else {
                let e = me - k;
                k + e / (1.0 + e * (le - 1.0) / ((1.0 - k) * (le - k)))
            }
        };
        assert!((curve(k) - k).abs() < 1e-6);
        assert!((curve(le) - 1.0).abs() < 1e-5);
        let mut prev = 0.0;
        for i in 0..=100 {
            let y = curve(le * i as f32 / 100.0);
            assert!(y >= prev, "monotonic");
            prev = y;
        }
        assert!((srgb_encode(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn letterbox_keeps_aspect() {
        assert_eq!(letterbox(1280, 720, 1280, 720), (0.0, 0.0, 1280.0, 720.0));
        assert_eq!(letterbox(1280, 720, 640, 360), (0.0, 0.0, 640.0, 360.0));
        // Taller source: pillarbox.
        assert_eq!(letterbox(1000, 1000, 1280, 720), (280.0, 0.0, 720.0, 720.0));
        // Wider source: letterbox.
        assert_eq!(letterbox(2560, 720, 1280, 720), (0.0, 180.0, 1280.0, 360.0));
        assert_eq!(letterbox(0, 0, 1280, 720), (0.0, 0.0, 1280.0, 720.0));
    }
}
