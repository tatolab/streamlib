// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's RHI [`PixelBuffer`] PluginAbiObject.
//!
//! Layout-stable `(handle, vtable, cached POD)` shape mirroring the engine's
//! `core/rhi/pixel_buffer.rs::PixelBuffer`. Arc refcount accounting runs in
//! host-compiled code via the vtable's `clone_pixel_buffer` /
//! `drop_pixel_buffer` callbacks; per-plane data reads route through
//! `plane_base_address_pixel_buffer` / `plane_size_pixel_buffer`. The host
//! `PixelBufferRef` backing + the `new` / `from_arc_into_raw` / `buffer_ref`
//! constructors stay in the engine.

use std::ffi::c_void;

use streamlib_consumer_rhi::PixelFormat;
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

/// Platform-agnostic identifier for a pooled pixel buffer.
///
/// Engine-free mirror of the engine's `PixelBufferPoolId` — a UUID string
/// returned by
/// [`crate::context::GpuContextLimitedAccess::acquire_pixel_buffer`] and
/// handed back to
/// [`crate::context::GpuContextLimitedAccess::get_pixel_buffer`]. Crosses the
/// plugin ABI as a UTF-8 byte buffer, not a `#[repr(C)]` value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PixelBufferPoolId(String);

impl PixelBufferPoolId {
    /// Wrap an existing id string (e.g. one returned across the plugin ABI).
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Wrap an id string slice.
    pub fn from_str(s: &str) -> Self {
        Self(s.to_string())
    }

    /// Borrow the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PixelBufferPoolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Pixel buffer with cached dimensions.
///
/// Layout-stable: every field is either a primitive or an opaque pointer.
/// Clone bumps the host's `Arc<PixelBufferRef>` strong count via
/// [`GpuContextLimitedAccessVTable::clone_pixel_buffer`]; Drop decrements via
/// [`GpuContextLimitedAccessVTable::drop_pixel_buffer`]. Both run in
/// host-compiled code regardless of the calling plugin.
#[repr(C)]
pub struct PixelBuffer {
    /// Opaque handle to the host's `Arc<PixelBufferRef>` (produced by
    /// `Arc::into_raw`).
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI Clone/Drop + plane-accessor dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
    /// Cached width in pixels (queried once at construction host-side).
    pub width: u32,
    /// Cached height in pixels (queried once at construction host-side).
    pub height: u32,
    /// Cached pixel format `#[repr(u32)]` FourCC discriminant.
    pub(crate) format_raw: u32,
    /// Cached plane count (always `>= 1`).
    pub(crate) plane_count_cached: u32,
}

// SAFETY: `handle` points at an `Arc<PixelBufferRef>` whose interior is
// Send+Sync. Refcount management crosses the plugin ABI through the vtable,
// but the underlying Arc bookkeeping runs in host-compiled code regardless.
unsafe impl Send for PixelBuffer {}
unsafe impl Sync for PixelBuffer {}

impl PixelBuffer {
    /// Cached pixel format. Captured at construction; pure field read with no
    /// plugin ABI dispatch.
    pub fn format(&self) -> PixelFormat {
        // Mirror of the engine `PixelBuffer::format()` FourCC mapping. There
        // is no portable `PixelFormat::from_raw` on consumer-rhi (the named
        // converter is macOS-only), so the canonical match is inlined.
        match self.format_raw {
            0x42475241 => PixelFormat::Bgra32,
            0x52474241 => PixelFormat::Rgba32,
            0x00000020 => PixelFormat::Argb32,
            0x52476841 => PixelFormat::Rgba64,
            0x34323076 => PixelFormat::Nv12VideoRange,
            0x34323066 => PixelFormat::Nv12FullRange,
            0x32767579 => PixelFormat::Uyvy422,
            0x79757673 => PixelFormat::Yuyv422,
            0x4C303038 => PixelFormat::Gray8,
            _ => PixelFormat::Unknown,
        }
    }

    /// Number of DMA-BUF planes backing this pixel buffer. Always `>= 1`.
    /// Cached at construction; pure field read.
    pub fn plane_count(&self) -> u32 {
        self.plane_count_cached
    }

    /// Mapped base address for the given plane, or null if out of range.
    /// Dispatches through the vtable's
    /// [`GpuContextLimitedAccessVTable::plane_base_address_pixel_buffer`]
    /// callback so the host's `PixelBufferRef` layout is never touched
    /// cdylib-side.
    pub fn plane_base_address(&self, plane_index: u32) -> *mut u8 {
        if self.handle.is_null() || self.vtable.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: vtable + handle paired at construction.
        unsafe { ((*self.vtable).plane_base_address_pixel_buffer)(self.handle, plane_index) }
    }

    /// Byte size of the given plane, or `0` if out of range. Dispatches
    /// through the vtable's
    /// [`GpuContextLimitedAccessVTable::plane_size_pixel_buffer`] callback.
    pub fn plane_size(&self, plane_index: u32) -> u64 {
        if self.handle.is_null() || self.vtable.is_null() {
            return 0;
        }
        // SAFETY: vtable + handle paired at construction.
        unsafe { ((*self.vtable).plane_size_pixel_buffer)(self.handle, plane_index) }
    }
}

impl Clone for PixelBuffer {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at construction; the vtable's
            // `clone_pixel_buffer` contract is `Arc::increment_strong_count`
            // host-side. Balanced by Drop.
            unsafe {
                ((*self.vtable).clone_pixel_buffer)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            width: self.width,
            height: self.height,
            format_raw: self.format_raw,
            plane_count_cached: self.plane_count_cached,
        }
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_pixel_buffer` bumps.
            unsafe {
                ((*self.vtable).drop_pixel_buffer)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for PixelBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format())
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn pixel_buffer_layout() {
        // Must match the engine's `core/rhi/pixel_buffer.rs::PixelBuffer`:
        //   handle @ 0, vtable @ 8, width @ 16, height @ 20,
        //   format_raw @ 24, plane_count_cached @ 28.
        // Total 32 bytes, align 8.
        assert_eq!(size_of::<PixelBuffer>(), 32);
        assert_eq!(align_of::<PixelBuffer>(), 8);
        assert_eq!(offset_of!(PixelBuffer, handle), 0);
        assert_eq!(offset_of!(PixelBuffer, vtable), 8);
        assert_eq!(offset_of!(PixelBuffer, width), 16);
        assert_eq!(offset_of!(PixelBuffer, height), 20);
        assert_eq!(offset_of!(PixelBuffer, format_raw), 24);
        assert_eq!(offset_of!(PixelBuffer, plane_count_cached), 28);
    }

    #[test]
    fn pixel_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PixelBuffer>();
    }
}
