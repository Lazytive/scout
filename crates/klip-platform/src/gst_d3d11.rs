//! The few functions of GStreamer's D3D11 library (`gstd3d11-1.0`, LGPL, linked
//! dynamically like the rest of GStreamer) that Klip needs to hand its own D3D11
//! textures to a pipeline: wrap our `ID3D11Device` as a `GstD3D11Device`, share it with
//! the pipeline's elements through a `GstContext`, allocate frames from a D3D11 buffer
//! pool, and serialise use of the immediate context with the device lock.
//!
//! Every pointer passed in comes from a live owner on our side (the windows crate's COM
//! wrappers or gstreamer-rs objects) and outlives the call; ownership of returned
//! objects follows GStreamer's transfer annotations (noted per function).

#![allow(unsafe_code)]

use std::ffi::c_void;

use gstreamer as gst;
use gstreamer::ffi as gst_ffi;
use gstreamer::glib::translate::{FromGlibPtrFull, ToGlibPtr};
use gstreamer::prelude::*;
use gstreamer_video as gst_video;
use gstreamer_video::ffi as video_ffi;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::core::Interface;

use crate::PlatformError;

/// `GST_MAP_D3D11` (`GST_MAP_FLAG_LAST << 1`): map the D3D11 texture, not system memory.
const GST_MAP_D3D11: u32 = (1 << 16) << 1;

#[repr(C)]
struct GstD3D11DeviceRaw {
    _private: [u8; 0],
}

#[repr(C)]
struct GstD3D11AllocationParamsRaw {
    _private: [u8; 0],
}

#[link(name = "gstd3d11-1.0")]
unsafe extern "C" {
    /// transfer full; NULL on failure.
    fn gst_d3d11_device_new_wrapped(device: *mut c_void) -> *mut GstD3D11DeviceRaw;
    /// transfer full.
    fn gst_d3d11_context_new(device: *mut GstD3D11DeviceRaw) -> *mut gst_ffi::GstContext;
    fn gst_d3d11_device_lock(device: *mut GstD3D11DeviceRaw);
    fn gst_d3d11_device_unlock(device: *mut GstD3D11DeviceRaw);
    /// transfer full.
    fn gst_d3d11_buffer_pool_new(device: *mut GstD3D11DeviceRaw) -> *mut gst_ffi::GstBufferPool;
    /// transfer full (free with `gst_d3d11_allocation_params_free`).
    fn gst_d3d11_allocation_params_new(
        device: *mut GstD3D11DeviceRaw,
        info: *const video_ffi::GstVideoInfo,
        flags: u32,
        bind_flags: u32,
        misc_flags: u32,
    ) -> *mut GstD3D11AllocationParamsRaw;
    fn gst_d3d11_allocation_params_free(params: *mut GstD3D11AllocationParamsRaw);
    /// Copies `params`.
    fn gst_buffer_pool_config_set_d3d11_allocation_params(
        config: *mut gst_ffi::GstStructure,
        params: *mut GstD3D11AllocationParamsRaw,
    );
}

/// A `GstD3D11Device` wrapping one of our `ID3D11Device`s (holds a reference).
pub(crate) struct GstDevice {
    raw: *mut GstD3D11DeviceRaw,
}

// SAFETY: GstD3D11Device is a GstObject: reference counting is atomic and its methods are
// thread-safe (the device lock exists to serialise the immediate context across threads).
unsafe impl Send for GstDevice {}
// SAFETY: as above; `&GstDevice` only allows the thread-safe calls below.
unsafe impl Sync for GstDevice {}

impl Drop for GstDevice {
    fn drop(&mut self) {
        // SAFETY: `raw` is the full reference from `gst_d3d11_device_new_wrapped`, released
        // exactly once.
        unsafe { gst_ffi::gst_object_unref(self.raw.cast()) };
    }
}

/// Holds the device lock; unlocks on drop.
pub(crate) struct DeviceLock<'a>(&'a GstDevice);

impl Drop for DeviceLock<'_> {
    fn drop(&mut self) {
        // SAFETY: pairs with the lock taken in `GstDevice::lock` on this thread.
        unsafe { gst_d3d11_device_unlock(self.0.raw) };
    }
}

impl GstDevice {
    /// Wrap `device` (GStreamer takes its own COM reference).
    pub(crate) fn wrap(device: &ID3D11Device) -> Result<GstDevice, PlatformError> {
        // SAFETY: `device` is a live ID3D11Device; the function AddRefs what it keeps.
        let raw = unsafe { gst_d3d11_device_new_wrapped(device.as_raw()) };
        if raw.is_null() {
            return Err(PlatformError::Os(
                "GStreamer could not wrap the D3D11 device".into(),
            ));
        }
        Ok(GstDevice { raw })
    }

    /// A context that makes the pipeline's D3D11 elements use this device.
    pub(crate) fn context(&self) -> gst::Context {
        // SAFETY: `raw` is live; the function returns a new (full) reference.
        unsafe { gst::Context::from_glib_full(gst_d3d11_context_new(self.raw)) }
    }

    /// Serialise use of the device's immediate context with GStreamer's threads.
    pub(crate) fn lock(&self) -> DeviceLock<'_> {
        // SAFETY: `raw` is live; unlocked by `DeviceLock::drop` on this thread.
        unsafe { gst_d3d11_device_lock(self.raw) };
        DeviceLock(self)
    }

    /// An active buffer pool of `width`x`height` textures in `format`, usable as render
    /// targets and shader inputs, holding `min..=max` buffers.
    pub(crate) fn texture_pool(
        &self,
        format: gst_video::VideoFormat,
        width: u32,
        height: u32,
        min: u32,
        max: u32,
        bind_flags: u32,
    ) -> Result<(gst::BufferPool, gst::Caps), PlatformError> {
        let err = |m: &str| PlatformError::Os(format!("D3D11 buffer pool: {m}"));
        let info = gst_video::VideoInfo::builder(format, width, height)
            .build()
            .map_err(|e| err(&e.to_string()))?;
        let mut caps = info.to_caps().map_err(|e| err(&e.to_string()))?;
        caps.get_mut()
            .ok_or_else(|| err("caps not writable"))?
            .set_features_simple(Some(gst::CapsFeatures::new(["memory:D3D11Memory"])));
        // SAFETY: `raw` is live; returns a new (full) reference to a GstBufferPool.
        let pool = unsafe { gst::BufferPool::from_glib_full(gst_d3d11_buffer_pool_new(self.raw)) };
        let mut config = pool.config();
        let size = u32::try_from(info.size()).map_err(|_| err("frame too large"))?;
        config.set_params(Some(&caps), size, min, max);
        // SAFETY: `info` is a valid GstVideoInfo for the duration of the call; the
        // returned params are freed below after the config copied them.
        let params = unsafe {
            gst_d3d11_allocation_params_new(self.raw, info.to_glib_none().0, 0, bind_flags, 0)
        };
        if params.is_null() {
            return Err(err("no allocation params"));
        }
        // SAFETY: `config` is the pool's writable config structure; `params` is valid and
        // copied by the call, then freed exactly once.
        unsafe {
            gst_buffer_pool_config_set_d3d11_allocation_params(config.as_mut_ptr(), params);
            gst_d3d11_allocation_params_free(params);
        }
        pool.set_config(config).map_err(|e| err(&e.to_string()))?;
        pool.set_active(true).map_err(|e| err(&e.to_string()))?;
        Ok((pool, caps))
    }
}

/// A D3D11 memory mapped for GPU writes: gives its texture; unmaps on drop (which tells
/// GStreamer the texture changed).
pub(crate) struct MappedTexture<'a> {
    mem: &'a mut gst::MemoryRef,
    info: gst_ffi::GstMapInfo,
    texture: ID3D11Texture2D,
}

impl MappedTexture<'_> {
    /// Map the first memory of `buffer` (must be D3D11 memory) for writing.
    pub(crate) fn map(buffer: &mut gst::BufferRef) -> Result<MappedTexture<'_>, PlatformError> {
        let mem = buffer
            .peek_memory_mut(0)
            .map_err(|_| PlatformError::Os("frame buffer is not writable".into()))?;
        // SAFETY: GstMapInfo is plain data that gst_memory_map fills in.
        let mut info: gst_ffi::GstMapInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `mem` is a live, writable GstMemory; `info` is a valid out pointer.
        let ok = unsafe {
            gst_ffi::gst_memory_map(
                mem.as_mut_ptr(),
                &raw mut info,
                gst_ffi::GST_MAP_WRITE | GST_MAP_D3D11,
            )
        };
        if ok == 0 || info.data.is_null() {
            return Err(PlatformError::Os("could not map the frame texture".into()));
        }
        let raw: *mut c_void = info.data.cast::<c_void>();
        // SAFETY: with GST_MAP_D3D11 the data pointer is the memory's ID3D11Texture2D,
        // valid while mapped; `from_raw_borrowed` + clone takes our own reference.
        let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }.cloned();
        let Some(texture) = texture else {
            // SAFETY: undo the successful map above.
            unsafe { gst_ffi::gst_memory_unmap(mem.as_mut_ptr(), &raw mut info) };
            return Err(PlatformError::Os("mapped frame has no texture".into()));
        };
        Ok(MappedTexture { mem, info, texture })
    }

    pub(crate) fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }
}

impl Drop for MappedTexture<'_> {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful gst_memory_map in `map`.
        unsafe { gst_ffi::gst_memory_unmap(self.mem.as_mut_ptr(), &raw mut self.info) };
    }
}
