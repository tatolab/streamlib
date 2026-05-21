// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer with cached dimensions.
//!
//! Phase C1 (#901) reshaped `PixelBuffer` to `(handle, vtable,
//! cached POD)` so the type is layout-stable across the cdylib DSO
//! boundary. The handle is `Arc::into_raw(Arc<PixelBufferRef>)`
//! produced by host code; the vtable's `clone_pixel_buffer` /
//! `drop_pixel_buffer` callbacks manage the Arc refcount in
//! host-compiled code, so Clone/Drop work correctly regardless of
//! the cdylib's compiled `Arc` layout.

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

use super::{PixelBufferRef, PixelFormat};

/// Pixel buffer with cached dimensions.
///
/// Layout-stable: every field is either a primitive or an opaque
/// pointer. The platform-specific `PixelBufferRef` is hidden behind
/// the opaque `handle`; engine-internal callers reach it through
/// [`PixelBuffer::buffer_ref`], cdylib callers route through the
/// vtable.
///
/// Clone only increments the host's `Arc<PixelBufferRef>` strong
/// count via [`GpuContextLimitedAccessVTable::clone_pixel_buffer`] —
/// it does NOT increment the platform buffer refcount (e.g.,
/// CVPixelBufferRetain on macOS). This is critical for avoiding
/// memory leaks when sharing buffers between Rust and Python.
///
/// The platform buffer is retained exactly once (when created) and
/// released exactly once (when the last `PixelBuffer` referencing
/// the underlying Arc is dropped).
#[repr(C)]
pub struct PixelBuffer {
    /// Opaque handle to the host's `Arc<PixelBufferRef>` (produced
    /// by `Arc::into_raw`). Engine-internal callers downcast to
    /// `*const PixelBufferRef` via [`PixelBuffer::buffer_ref`];
    /// cdylib callers treat it as opaque.
    pub(crate) handle: *const c_void,
    /// Vtable for cross-DSO Clone/Drop dispatch. In host mode this
    /// points at `&HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`; in
    /// cdylib mode it's the host-installed pointer from
    /// `HostServices::gpu_context_limited_access_vtable`.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
    /// Cached width (queried once at construction).
    pub width: u32,
    /// Cached height (queried once at construction).
    pub height: u32,
    /// Cached pixel format (queried once at construction).
    ///
    /// Stored as the `#[repr(u32)]` discriminant of [`PixelFormat`]
    /// so the field is ABI-stable across the cdylib boundary even
    /// if `PixelFormat`'s Rust-level layout were to drift. Read back
    /// via [`PixelBuffer::format`] which casts through the well-
    /// defined `repr(u32)` mapping.
    pub(crate) format_raw: u32,
    /// Cached plane count (queried once at construction).
    ///
    /// Always `>= 1`. Mirrors `slpn_gpu_surface_plane_count` /
    /// `sldn_gpu_surface_plane_count` on the polyglot shim side so
    /// cdylib callers reach the same value through a pure field
    /// read instead of a vtable round-trip.
    pub(crate) plane_count_cached: u32,
}

// SAFETY: `handle` points at an `Arc<PixelBufferRef>` whose interior
// is Send+Sync (the platform buffer types — `HostVulkanBuffer` and
// `CVPixelBufferRef` — are themselves Send+Sync). Refcount management
// crosses the cdylib boundary through the vtable, but the underlying
// Arc bookkeeping runs in host-compiled code regardless.
unsafe impl Send for PixelBuffer {}
unsafe impl Sync for PixelBuffer {}

impl PixelBuffer {
    /// Create from a platform buffer reference. Queries width,
    /// height, format, and plane count from the platform once and
    /// caches them, then leaks an initial Arc strong count via
    /// `Arc::into_raw` so the `PixelBuffer`'s Drop is balanced by
    /// exactly one decrement.
    pub fn new(ref_: PixelBufferRef) -> Self {
        let width = ref_.width();
        let height = ref_.height();
        let format = ref_.format();
        let plane_count = ref_.plane_count();
        let arc = Arc::new(ref_);
        Self::from_arc_into_raw(arc, width, height, format, plane_count)
    }

    /// Wrap an externally-allocated `Arc<HostVulkanBuffer>` with the
    /// pixel-shape metadata the caller knows about so it can be passed
    /// to host-side APIs that take `&PixelBuffer` (e.g.
    /// [`crate::core::context::SurfaceStore::register_pixel_buffer_with_timeline`])
    /// without going through the [`crate::core::context::PixelBufferPoolManager`].
    /// Used by application setup code that wants to allocate a staging
    /// buffer directly via the RHI and register it with a surface_id of
    /// its own choosing.
    ///
    /// `HostVulkanBuffer` is the generic Vulkan buffer allocation
    /// primitive and carries no pixel semantics; pixel `width` /
    /// `height` / `bytes_per_pixel` / `format` live on this wrapper.
    #[cfg(target_os = "linux")]
    pub fn from_host_vulkan_buffer(
        buffer: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
    ) -> Self {
        let plane_count = buffer.plane_count();
        let arc = Arc::new(PixelBufferRef {
            inner: buffer,
            width,
            height,
            bytes_per_pixel,
            format,
        });
        Self::from_arc_into_raw(arc, width, height, format, plane_count)
    }

    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw`, capture the host-mode vtable, and build the
    /// `(handle, vtable, POD)` shape.
    fn from_arc_into_raw(
        arc: Arc<PixelBufferRef>,
        width: u32,
        height: u32,
        format: PixelFormat,
        plane_count: u32,
    ) -> Self {
        let handle = Arc::into_raw(arc) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self {
            handle,
            vtable,
            width,
            height,
            format_raw: format as u32,
            plane_count_cached: plane_count,
        }
    }

    /// Cached pixel format. Captured at construction; pure field
    /// read with no cross-DSO dispatch.
    pub fn format(&self) -> PixelFormat {
        // SAFETY: `format_raw` is the `#[repr(u32)]` discriminant of
        // a `PixelFormat` value that was alive at construction time
        // (captured via `format as u32`). The mapping is the
        // identity round-trip the `repr(u32)` enum guarantees;
        // unknown discriminants are mapped to `PixelFormat::Unknown`
        // via the public `from_cv_pixel_format_type` on macOS or by
        // the fallback below — but we never store an unknown
        // discriminant here because `format_raw` was sourced from a
        // valid `PixelFormat` value.
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

    /// Borrow the underlying [`PixelBufferRef`]. Engine-internal —
    /// cdylib code reaches platform-specific data through
    /// [`crate::host_rhi::HostPixelBufferRefExt`] which itself is
    /// engine-only.
    pub fn buffer_ref(&self) -> &PixelBufferRef {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<PixelBufferRef>)`
        // (see `from_arc_into_raw`). The leaked strong count keeps
        // the `PixelBufferRef` alive at least until `Drop` runs.
        unsafe { &*(self.handle as *const PixelBufferRef) }
    }

    /// Number of DMA-BUF planes backing this pixel buffer. Always `>= 1`.
    /// Mirror of `slpn_gpu_surface_plane_count` on the polyglot shims.
    /// Cached at construction; pure field read with no cross-DSO
    /// dispatch.
    pub fn plane_count(&self) -> u32 {
        self.plane_count_cached
    }

    /// Mapped base address for the given plane, or null if out of range.
    /// Plane 0 on a VMA-allocated or single-plane-imported buffer points
    /// at the same bytes as [`mapped_ptr`](PixelBufferRef::plane_base_address)
    /// with index 0.
    ///
    /// Dispatches through the vtable's
    /// [`plane_base_address_pixel_buffer`](GpuContextLimitedAccessVTable::plane_base_address_pixel_buffer)
    /// callback so the host's `PixelBufferRef` layout is never touched
    /// cdylib-side.
    pub fn plane_base_address(&self, plane_index: u32) -> *mut u8 {
        if self.handle.is_null() || self.vtable.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: vtable + handle were paired at construction by
        // `from_arc_into_raw`; the callback's contract is documented
        // on the vtable field.
        unsafe { ((*self.vtable).plane_base_address_pixel_buffer)(self.handle, plane_index) }
    }

    /// Byte size of the given plane, or `0` if out of range. Dispatches
    /// through the vtable's
    /// [`plane_size_pixel_buffer`](GpuContextLimitedAccessVTable::plane_size_pixel_buffer)
    /// callback.
    pub fn plane_size(&self, plane_index: u32) -> u64 {
        if self.handle.is_null() || self.vtable.is_null() {
            return 0;
        }
        // SAFETY: vtable + handle were paired at construction by
        // `from_arc_into_raw`.
        unsafe { ((*self.vtable).plane_size_pixel_buffer)(self.handle, plane_index) }
    }

    /// Get the raw platform pointer (CVPixelBufferRef on macOS).
    #[cfg(target_os = "macos")]
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.buffer_ref().as_ptr()
    }

    /// Number of `PixelBuffer` references to the same underlying
    /// `PixelBufferRef`. Engine-internal — used by the pool manager
    /// to detect "buffer no longer in use" without locking.
    ///
    /// Counts `PixelBuffer` clones, NOT the underlying platform
    /// buffer's retain count (e.g. CVPixelBufferRetain). A platform
    /// buffer referenced by one `PixelBuffer` returns `strong_count
    /// == 1` even if the platform's own refcount is higher.
    ///
    /// Dispatches through the vtable's
    /// [`strong_count_pixel_buffer`](GpuContextLimitedAccessVTable::strong_count_pixel_buffer)
    /// callback so the host's `Arc<PixelBufferRef>` accounting runs
    /// in host-compiled code regardless of caller DSO.
    pub(crate) fn strong_count(&self) -> usize {
        if self.handle.is_null() || self.vtable.is_null() {
            return 0;
        }
        // SAFETY: vtable + handle were paired at construction by
        // `from_arc_into_raw`; the callback's contract is documented
        // on the vtable field.
        unsafe { ((*self.vtable).strong_count_pixel_buffer)(self.handle) }
    }
}

impl Clone for PixelBuffer {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle were paired at construction by
            // `from_arc_into_raw`; the vtable's `clone_pixel_buffer`
            // contract is `Arc::increment_strong_count(handle)` on
            // the host side. Balanced by the Drop impl below.
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
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `clone_pixel_buffer` bumps.
            // `drop_pixel_buffer` decrements the host-side Arc; when
            // refcount hits zero the underlying `PixelBufferRef` is
            // freed in host-compiled code.
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

// =============================================================================
// Layout regression tests
// =============================================================================
//
// `PixelBuffer` is the load-bearing β-reshape type that crosses the
// cdylib DSO boundary. A drift in its `#[repr(C)]` layout would
// silently corrupt every `acquire_pixel_buffer` / `release_pixel_buffer`
// / `resolve_pixel_buffer_by_surface_id` round-trip — the host's
// pixel-buffer accessors would read the cdylib's stale field offsets.
// The vtable layout-version constant
// (`GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION`) catches drift
// in the dispatch table; this test catches drift in the value type
// itself.
//
// Sister tests at `libs/streamlib-plugin-abi/src/lib.rs::layout_tests`
// pin the vtable structs the same way.

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn pixel_buffer_layout() {
        // Phase 0 hardening (#901): pin the byte-level shape of the
        // cross-DSO `PixelBuffer`. Fields:
        //   handle              : *const c_void  → offset 0,  size 8
        //   vtable              : *const VTable  → offset 8,  size 8
        //   width               : u32            → offset 16, size 4
        //   height              : u32            → offset 20, size 4
        //   format_raw          : u32            → offset 24, size 4
        //   plane_count_cached  : u32            → offset 28, size 4
        // Total: 32 bytes, 8-byte alignment (pinned by the pointer fields).
        assert_eq!(size_of::<PixelBuffer>(), 32);
        assert_eq!(align_of::<PixelBuffer>(), 8);
        assert_eq!(offset_of!(PixelBuffer, handle), 0);
        assert_eq!(offset_of!(PixelBuffer, vtable), 8);
        assert_eq!(offset_of!(PixelBuffer, width), 16);
        assert_eq!(offset_of!(PixelBuffer, height), 20);
        assert_eq!(offset_of!(PixelBuffer, format_raw), 24);
        assert_eq!(offset_of!(PixelBuffer, plane_count_cached), 28);
    }

    /// Compile-time witness that `PixelBuffer` is Send + Sync. The
    /// raw pointer fields would otherwise prevent auto-derive; the
    /// `unsafe impl Send + Sync` is sound only because Arc refcount
    /// management runs in host-compiled code via the vtable
    /// callbacks.
    #[test]
    fn pixel_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PixelBuffer>();
    }
}
