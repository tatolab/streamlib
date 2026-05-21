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
}

// SAFETY: `handle` points at an `Arc<PixelBufferRef>` whose interior
// is Send+Sync (the platform buffer types — `HostVulkanBuffer` and
// `CVPixelBufferRef` — are themselves Send+Sync). Refcount management
// crosses the cdylib boundary through the vtable, but the underlying
// Arc bookkeeping runs in host-compiled code regardless.
unsafe impl Send for PixelBuffer {}
unsafe impl Sync for PixelBuffer {}

impl PixelBuffer {
    /// Create from a platform buffer reference. Queries width and
    /// height from the platform once and caches them, then leaks an
    /// initial Arc strong count via `Arc::into_raw` so the
    /// `PixelBuffer`'s Drop is balanced by exactly one decrement.
    pub fn new(ref_: PixelBufferRef) -> Self {
        let width = ref_.width();
        let height = ref_.height();
        let arc = Arc::new(ref_);
        Self::from_arc_into_raw(arc, width, height)
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
        let arc = Arc::new(PixelBufferRef {
            inner: buffer,
            width,
            height,
            bytes_per_pixel,
            format,
        });
        Self::from_arc_into_raw(arc, width, height)
    }

    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw`, capture the host-mode vtable, and build the
    /// `(handle, vtable, POD)` shape.
    fn from_arc_into_raw(arc: Arc<PixelBufferRef>, width: u32, height: u32) -> Self {
        let handle = Arc::into_raw(arc) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self {
            handle,
            vtable,
            width,
            height,
        }
    }

    /// Query the pixel format from the platform.
    pub fn format(&self) -> PixelFormat {
        self.buffer_ref().format()
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
    pub fn plane_count(&self) -> u32 {
        self.buffer_ref().plane_count()
    }

    /// Mapped base address for the given plane, or null if out of range.
    /// Plane 0 on a VMA-allocated or single-plane-imported buffer points
    /// at the same bytes as [`mapped_ptr`](PixelBufferRef::plane_base_address)
    /// with index 0.
    pub fn plane_base_address(&self, plane_index: u32) -> *mut u8 {
        self.buffer_ref().plane_base_address(plane_index)
    }

    /// Byte size of the given plane, or `0` if out of range.
    pub fn plane_size(&self, plane_index: u32) -> u64 {
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
    pub(crate) fn strong_count(&self) -> usize {
        if self.handle.is_null() {
            return 0;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`
        // (see `from_arc_into_raw`). Temporarily reconstruct the
        // Arc to query the count, then re-leak via `Arc::into_raw`
        // so the strong count stays balanced — there is no public
        // `Arc::strong_count_from_raw` API.
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
