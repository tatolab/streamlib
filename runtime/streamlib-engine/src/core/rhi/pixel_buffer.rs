// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer with cached dimensions.
//!
//! `(handle, cached POD)` shape: the handle is
//! `Arc::into_raw(Arc<PixelBufferRef>)`; Clone/Drop refcount it directly.

use std::ffi::c_void;
use std::sync::Arc;


use super::{PixelBufferRef, PixelFormat};

/// Pixel buffer with cached dimensions.
///
/// Layout-stable: every field is either a primitive or an opaque
/// pointer. The platform-specific `PixelBufferRef` is hidden behind
/// the opaque `handle`; engine-internal callers reach it through
/// [`PixelBuffer::buffer_ref`].
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
    /// `*const PixelBufferRef` via [`PixelBuffer::buffer_ref`].
    pub(crate) handle: *const c_void,
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
        Self {
            handle,
            width,
            height,
            format_raw: format as u32,
            plane_count_cached: plane_count,
        }
    }

    /// Cached pixel format. Captured at construction; pure field read.
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

    /// Borrow the underlying [`PixelBufferRef`]. **Engine-only.**
    /// Cdylib code reaches platform-specific data through
    /// [`crate::host_rhi::HostPixelBufferRefExt`] which itself is
    /// engine-only.
    ///
    /// Panics if reached from cdylib code (#908). `PixelBufferRef`
    /// is a host-internal type whose layout is rustc-version-
    /// dependent — dereffing the handle as `*const PixelBufferRef`
    /// from a cdylib compiled with a different rustc would be
    /// undefined behavior. The panic guard turns the UB into a
    /// clean abort.
    pub fn buffer_ref(&self) -> &PixelBufferRef {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<PixelBufferRef>)`
        // (see `from_arc_into_raw`). The leaked strong count keeps
        // the `PixelBufferRef` alive at least until `Drop` runs. The
        // panic guard above rejects cdylib callers so the deref runs
        // only in host-compiled code where `PixelBufferRef`'s layout
        // is known.
        unsafe { &*(self.handle as *const PixelBufferRef) }
    }

    /// Number of DMA-BUF planes backing this pixel buffer. Always `>= 1`.
    /// Mirror of `slpn_gpu_surface_plane_count` on the polyglot shims.
    /// Cached at construction; pure field read.
    pub fn plane_count(&self) -> u32 {
        self.plane_count_cached
    }

    /// Mapped base address for the given plane, or null if out of range.
    /// Plane 0 on a VMA-allocated or single-plane-imported buffer points
    /// at the same bytes as [`mapped_ptr`](PixelBufferRef::plane_base_address)
    /// with index 0.
    ///
    pub fn plane_base_address(&self, plane_index: u32) -> *mut u8 {
        if self.handle.is_null() {
            return core::ptr::null_mut();
        }
        self.buffer_ref().plane_base_address(plane_index)
    }

    /// Byte size of the given plane, or `0` if out of range.
    pub fn plane_size(&self, plane_index: u32) -> u64 {
        if self.handle.is_null() {
            return 0;
        }
        self.buffer_ref().plane_size(plane_index)
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
    pub(crate) fn strong_count(&self) -> usize {
        if self.handle.is_null() {
            return 0;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)` (see
        // `from_arc_into_raw`). The Arc is reconstructed to read the count
        // and immediately re-leaked, so the strong count returns to its
        // pre-call value — `Arc::strong_count_from_raw` is not stable.
        unsafe {
            let arc = Arc::from_raw(self.handle as *const PixelBufferRef);
            let count = Arc::strong_count(&arc);
            let _ = Arc::into_raw(arc);
            count
        }
    }
}

impl Clone for PixelBuffer {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`
            // (see `from_arc_into_raw`); balanced by the Drop impl below.
            unsafe {
                Arc::increment_strong_count(self.handle as *const PixelBufferRef);
            }
        }
        Self {
            handle: self.handle,
            width: self.width,
            height: self.height,
            format_raw: self.format_raw,
            plane_count_cached: self.plane_count_cached,
        }
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `Clone` increment. When the
            // refcount hits zero the `PixelBufferRef` is freed.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const PixelBufferRef);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time witness that `PixelBuffer` is Send + Sync.
    #[test]
    fn pixel_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PixelBuffer>();
    }
}
