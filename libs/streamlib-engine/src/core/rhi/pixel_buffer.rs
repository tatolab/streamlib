// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer with cached dimensions.

use std::sync::Arc;

use super::{PixelFormat, PixelBufferRef};

/// Pixel buffer with cached dimensions.
///
/// Wraps [`PixelBufferRef`] in an Arc for cheap cloning. Clone only increments
/// the Arc refcount - it does NOT increment the platform buffer refcount (e.g.,
/// CVPixelBufferRetain on macOS). This is critical for avoiding memory leaks
/// when sharing buffers between Rust and Python.
///
/// The platform buffer is retained exactly once (when created) and released
/// exactly once (when the last Arc reference is dropped).
#[derive(Clone)]
pub struct PixelBuffer {
    /// The underlying platform buffer reference, shared via Arc.
    /// Clone increments Arc refcount, NOT platform refcount.
    pub(crate) ref_: Arc<PixelBufferRef>,
    /// Cached width (queried once at construction).
    pub width: u32,
    /// Cached height (queried once at construction).
    pub height: u32,
}

impl PixelBuffer {
    /// Create from a platform buffer reference.
    ///
    /// Queries width and height from the platform once and caches them.
    /// The PixelBufferRef is wrapped in Arc for cheap cloning.
    pub fn new(ref_: PixelBufferRef) -> Self {
        let width = ref_.width();
        let height = ref_.height();
        Self {
            ref_: Arc::new(ref_),
            width,
            height,
        }
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
        Self {
            ref_: Arc::new(PixelBufferRef {
                inner: buffer,
                width,
                height,
                bytes_per_pixel,
                format,
            }),
            width,
            height,
        }
    }

    /// Query the pixel format from the platform.
    pub fn format(&self) -> PixelFormat {
        self.ref_.format()
    }

    /// Get the underlying buffer reference.
    pub fn buffer_ref(&self) -> &PixelBufferRef {
        &self.ref_
    }

    /// Number of DMA-BUF planes backing this pixel buffer. Always `>= 1`.
    /// Mirror of `slpn_gpu_surface_plane_count` on the polyglot shims.
    pub fn plane_count(&self) -> u32 {
        self.ref_.plane_count()
    }

    /// Mapped base address for the given plane, or null if out of range.
    /// Plane 0 on a VMA-allocated or single-plane-imported buffer points
    /// at the same bytes as [`mapped_ptr`](PixelBufferRef::plane_base_address)
    /// with index 0.
    pub fn plane_base_address(&self, plane_index: u32) -> *mut u8 {
        self.ref_.plane_base_address(plane_index)
    }

    /// Byte size of the given plane, or `0` if out of range.
    pub fn plane_size(&self, plane_index: u32) -> u64 {
        self.ref_.plane_size(plane_index)
    }

    /// Get the raw platform pointer (CVPixelBufferRef on macOS).
    #[cfg(target_os = "macos")]
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.ref_.as_ptr()
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

// Send + Sync inherited from Arc<PixelBufferRef>
