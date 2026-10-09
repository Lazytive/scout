//! WASAPI audio capture with QueryPerformanceCounter timestamps.
//!
//! Every packet `IAudioCaptureClient` hands out carries the QPC time its first frame was
//! captured. QPC is also the clock behind WGC frame times and GStreamer's system clock on
//! Windows, so stamping audio from it keeps it in sync with video no matter how the audio
//! was buffered on the way (GStreamer's `wasapi2src` stamped on arrival and was off by
//! −20 to +23 ms depending on the source type and the run).
//!
//! Output is always 32-bit float at the requested rate and channel count (WASAPI converts,
//! `AUTOCONVERTPCM`). When a stream delivers nothing (system loopback while nothing plays,
//! a device that failed or vanished), silence is generated on the QPC timeline so the
//! stream stays continuous, and a failed device is retried every two seconds.
//!
//! Every `unsafe` block is a call into a documented COM/Win32 API with arguments owned by
//! this module.

#![allow(unsafe_code)]

use crate::timeline::{Format, RETRY_NS, deliver, fill_silence};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, EDataFlow, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, HSTRING, IUnknown, Interface, Ref, implement};

use crate::{AudioCaptureSink, CaptureEndpoint, PlatformError};

/// WASAPI buffer length (100 ns units): 200 ms.
const BUFFER_HNS: i64 = 2_000_000;
/// Longest wait for data before checking for stop / silence.
const WAIT_MS: u32 = 10;
/// IEEE float sample format tag.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// Windows 10 version 2004, the first with process loopback.
const PROCESS_LOOPBACK_BUILD: u32 = 19041;

/// Current QueryPerformanceCounter time in nanoseconds.
pub(crate) fn qpc_now_ns() -> i64 {
    let mut count = 0i64;
    let mut freq = 0i64;
    // SAFETY: both arguments point to locals that outlive the calls.
    let ok = unsafe {
        QueryPerformanceCounter(&raw mut count).is_ok()
            && QueryPerformanceFrequency(&raw mut freq).is_ok()
    };
    if !ok || freq <= 0 {
        return 0;
    }
    let ns = i128::from(count)
        .saturating_mul(1_000_000_000)
        .checked_div(i128::from(freq))
        .unwrap_or(0);
    i64::try_from(ns).unwrap_or(i64::MAX)
}

/// Whether this Windows build supports per-process loopback capture.
pub(crate) fn process_loopback_supported() -> bool {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(std::mem::size_of::<OSVERSIONINFOW>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `info` is a writable OSVERSIONINFOW with its size set; RtlGetVersion only
    // fills it in.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    status.is_ok() && (info.dwMajorVersion > 10 || info.dwBuildNumber >= PROCESS_LOOPBACK_BUILD)
}

impl Format {
    fn wave_format(self) -> WAVEFORMATEX {
        let block = self.channels.saturating_mul(4);
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: self.channels,
            nSamplesPerSec: self.rate,
            nAvgBytesPerSec: self.rate.saturating_mul(u32::from(block)),
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: 0,
        }
    }
}

/// A running capture thread.
pub(crate) struct Handle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    pub(crate) fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(crate) fn start(
    endpoint: CaptureEndpoint,
    rate: u32,
    channels: u16,
    mut sink: Box<dyn AudioCaptureSink>,
) -> Result<Handle, PlatformError> {
    if rate == 0 || channels == 0 {
        return Err(PlatformError::Os("bad audio format".into()));
    }
    let fmt = Format { rate, channels };
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("klip-audio-capture".into())
        .spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&endpoint, fmt, &flag, sink.as_mut());
            }));
            if res.is_err() {
                sink.notice("internal error in audio capture; it stopped".into());
            }
        })
        .map_err(|e| PlatformError::Os(format!("audio thread: {e}")))?;
    Ok(Handle {
        stop,
        thread: Some(thread),
    })
}

/// Uninitialises COM on drop if this thread's `CoInitializeEx` succeeded.
struct Com(bool);

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs with the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn run(
    endpoint: &CaptureEndpoint,
    fmt: Format,
    stop: &AtomicBool,
    sink: &mut dyn AudioCaptureSink,
) {
    // SAFETY: CoInitializeEx may be called on any thread; see `Com`.
    let _com = Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());
    let mut next = Some(qpc_now_ns());
    let mut stream: Option<Stream> = None;
    let mut retry_at = 0i64;
    let mut failed = false;
    while !stop.load(Ordering::Relaxed) {
        let now = qpc_now_ns();
        if stream.is_none() && now >= retry_at {
            match Stream::open(endpoint, fmt) {
                Ok(s) => {
                    if failed {
                        sink.notice("audio capture is back".into());
                    }
                    failed = false;
                    stream = Some(s);
                }
                Err(e) => {
                    if !failed {
                        sink.notice(format!("audio capture unavailable, recording silence: {e}"));
                    }
                    failed = true;
                    retry_at = now.saturating_add(RETRY_NS);
                }
            }
        }
        if let Some(s) = &stream
            && let Err(e) = s.read(fmt, &mut next, sink)
        {
            sink.notice(format!("audio capture stopped, recording silence: {e}"));
            stream = None;
            failed = true;
            retry_at = now.saturating_add(RETRY_NS);
        }
        fill_silence(fmt, &mut next, sink);
        match &stream {
            Some(s) => s.wait(),
            None => std::thread::sleep(std::time::Duration::from_millis(u64::from(WAIT_MS))),
        }
    }
}

/// Signals an event when process-loopback activation finishes.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationDone {
    event: HANDLE,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationDone_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // SAFETY: the event outlives the activation: `activate_process` waits for this
        // signal (or times out) before closing it, and a late signal on a closed handle
        // only fails.
        unsafe { SetEvent(self.event) }
    }
}

/// An event handle closed on drop.
struct Event(HANDLE);

impl Event {
    fn new() -> windows::core::Result<Event> {
        // SAFETY: an unnamed auto-reset event with default security; closed in Drop.
        unsafe { CreateEventW(None, false, false, None) }.map(Event)
    }

    fn wait(&self, ms: u32) -> bool {
        // SAFETY: the handle is a live event owned by `self`.
        unsafe { WaitForSingleObject(self.0, ms) }.0 == 0
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: closes the handle created in `new`, exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn activate_device(flow: EDataFlow, device: Option<&str>) -> windows::core::Result<IAudioClient> {
    // SAFETY: standard in-process activation of the MMDevice enumerator.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
    let dev = match device {
        // SAFETY: live enumerator; the id string lives across the call.
        Some(id) => unsafe { enumerator.GetDevice(&HSTRING::from(id)) }?,
        // SAFETY: live enumerator, valid flow and role.
        None => unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }?,
    };
    // SAFETY: live device; IAudioClient is a documented activation target.
    unsafe { dev.Activate::<IAudioClient>(CLSCTX_ALL, None) }
}

fn activate_process(pid: u32, include: bool) -> windows::core::Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: if include {
                    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                } else {
                    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                },
            },
        },
    };
    // A VT_BLOB pointing at `params`. ManuallyDrop: the windows crate's `Drop` for
    // PROPVARIANT calls PropVariantClear, which would CoTaskMemFree our stack struct
    // (that was a heap corruption, caught by `audio_capture_is_continuous_and_qpc_stamped`).
    let prop = std::mem::ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: u32::try_from(std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>())
                            .unwrap_or(0),
                        pBlobData: (&raw const params).cast_mut().cast::<u8>(),
                    },
                },
            }),
        },
    });
    let done = Event::new()?;
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationDone { event: done.0 }.into();
    // SAFETY: `prop` (a VT_BLOB pointing at `params`) and `handler` live until the wait
    // below; the IID is IAudioClient's.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const *prop),
            &handler,
        )
    }?;
    if !done.wait(5_000) {
        return Err(windows::core::Error::new(
            HRESULT(0x8000_000E_u32 as i32),
            "process audio activation timed out",
        ));
    }
    let mut result = HRESULT(0);
    let mut unknown: Option<IUnknown> = None;
    // SAFETY: the operation has completed (event signalled); out pointers are locals.
    unsafe { operation.GetActivateResult(&raw mut result, &raw mut unknown) }?;
    result.ok()?;
    unknown
        .ok_or_else(|| windows::core::Error::new(HRESULT(0x8000_4005_u32 as i32), "no client"))?
        .cast()
}

/// An initialised, started capture client.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: Event,
}

impl Stream {
    fn open(endpoint: &CaptureEndpoint, fmt: Format) -> windows::core::Result<Stream> {
        let (client, loopback) = match endpoint {
            CaptureEndpoint::Loopback { device } => {
                (activate_device(eRender, device.as_deref())?, true)
            }
            CaptureEndpoint::Input { device } => {
                (activate_device(eCapture, device.as_deref())?, false)
            }
            CaptureEndpoint::Process { pid, include } => (activate_process(*pid, *include)?, true),
        };
        let wfx = fmt.wave_format();
        let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        // SAFETY: live client; `wfx` is a complete WAVEFORMATEX that outlives the call.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                BUFFER_HNS,
                0,
                &raw const wfx,
                None,
            )
        }?;
        let event = Event::new()?;
        // SAFETY: live, initialised client; the event lives as long as the stream.
        unsafe { client.SetEventHandle(event.0) }?;
        // SAFETY: live, initialised client.
        let capture: IAudioCaptureClient = unsafe { client.GetService() }?;
        // SAFETY: live, initialised client.
        unsafe { client.Start() }?;
        Ok(Stream {
            client,
            capture,
            event,
        })
    }

    fn wait(&self) {
        let _ = self.event.wait(WAIT_MS);
    }

    /// Hand every available packet to the sink.
    fn read(
        &self,
        fmt: Format,
        next: &mut Option<i64>,
        sink: &mut dyn AudioCaptureSink,
    ) -> windows::core::Result<()> {
        let channels = usize::from(fmt.channels);
        let mut samples: Vec<f32> = Vec::new();
        loop {
            // SAFETY: live capture client.
            if unsafe { self.capture.GetNextPacketSize() }? == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags, mut qpc) = (0u32, 0u32, 0u64);
            // SAFETY: out pointers to locals; the buffer stays valid until ReleaseBuffer.
            unsafe {
                self.capture.GetBuffer(
                    &raw mut data,
                    &raw mut frames,
                    &raw mut flags,
                    None,
                    Some(&raw mut qpc),
                )
            }?;
            let has = |f: i32| flags & u32::try_from(f).unwrap_or(0) != 0;
            let silent = has(AUDCLNT_BUFFERFLAGS_SILENT.0) || data.is_null();
            let start = if has(AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0) || qpc == 0 {
                next.unwrap_or_else(|| qpc_now_ns().saturating_sub(fmt.frames_ns(frames)))
            } else {
                i64::try_from(qpc).unwrap_or(i64::MAX).saturating_mul(100)
            };
            samples.clear();
            if !silent {
                let bytes = usize::try_from(frames)
                    .unwrap_or(0)
                    .saturating_mul(channels)
                    .saturating_mul(4);
                // SAFETY: WASAPI guarantees `frames` frames of the stream format (32-bit
                // float x channels) at `data` until ReleaseBuffer; read as bytes so
                // alignment doesn't matter.
                let raw = unsafe { std::slice::from_raw_parts(data, bytes) };
                samples.extend(
                    raw.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b)),
                );
            }
            deliver(
                fmt,
                next,
                start,
                frames,
                (!silent).then_some(samples.as_slice()),
                has(AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0),
                sink,
            );
            // SAFETY: releases exactly the frames obtained by GetBuffer above.
            unsafe { self.capture.ReleaseBuffer(frames) }?;
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: live client; stopping a stopped client is harmless.
        let _ = unsafe { self.client.Stop() };
    }
}
