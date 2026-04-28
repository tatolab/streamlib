// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform pixel buffer reference.

use super::PixelFormat;

/// Platform pixel buffer reference.
///
/// Wraps the platform's native pixel buffer type:
/// - macOS/iOS: CVPixelBufferRef (raw pointer, platform manages lifecycle)
/// - Linux: Arc\<HostVulkanPixelBuffer\> (shared GPU staging buffer, Rust manages lifecycle)
/// - Windows: ID3D11Texture2D* (future)
///
/// Clone increments the appropriate refcount, Drop decrements it.
/// No image data is ever copied.
pub struct RhiPixelBufferRef {
    #[cfg(target_os = "macos")]
    pub(crate) inner: std::ptr::NonNull<std::ffi::c_void>,

    #[cfg(target_os = "linux")]
    pub(crate) inner: std::sync::Arc<crate::vulkan::rhi::HostVulkanPixelBuffer>,

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) _marker: std::marker::PhantomData<()>,
}

impl RhiPixelBufferRef {
    /// Query the pixel format from the platform.
    pub fn format(&self) -> PixelFormat {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::format_impl(self)
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.format()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            PixelFormat::Unknown
        }
    }

    /// Query the width from the platform.
    pub fn width(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::width_impl(self)
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.width()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            0
        }
    }

    /// Query the height from the platform.
    pub fn height(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::height_impl(self)
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.height()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            0
        }
    }

    /// Number of DMA-BUF planes backing this pixel buffer.
    ///
    /// `1` for VMA-allocated buffers and single-plane DMA-BUF imports.
    /// `N` for multi-plane imports — mirror of
    /// `slpn_gpu_surface_plane_count` / `sldn_gpu_surface_plane_count`
    /// on the polyglot shim side. On macOS and unsupported platforms
    /// this always reports `1`.
    pub fn plane_count(&self) -> u32 {
        #[cfg(target_os = "linux")]
        {
            self.inner.plane_count()
        }
        #[cfg(not(target_os = "linux"))]
        {
            1
        }
    }

    /// Mapped base address for `plane_index`, or null if the plane index
    /// is out of range or the backend doesn't expose CPU-mapped planes.
    pub fn plane_base_address(&self, plane_index: u32) -> *mut u8 {
        #[cfg(target_os = "linux")]
        {
            self.inner.plane_mapped_ptr(plane_index)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = plane_index;
            std::ptr::null_mut()
        }
    }

    /// Byte size of `plane_index`, or `0` if the plane index is out of
    /// range.
    pub fn plane_size(&self, plane_index: u32) -> u64 {
        #[cfg(target_os = "linux")]
        {
            self.inner.plane_size(plane_index) as u64
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = plane_index;
            0
        }
    }

    /// Get the raw platform pointer (CVPixelBufferRef on macOS).
    #[cfg(target_os = "macos")]
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.inner.as_ptr()
    }

    /// Adapter-facing: the underlying [`crate::vulkan::rhi::HostVulkanPixelBuffer`].
    ///
    /// In-tree surface adapters (`streamlib-adapter-cpu-readback`, others
    /// that need to issue `vkCmdCopyImageToBuffer` /
    /// `vkCmdCopyBufferToImage` against a HOST_VISIBLE staging buffer)
    /// need direct access to the `vk::Buffer` handle plus the mapped
    /// pointer. Customers and non-adapter code must NOT call this — the
    /// engine boundary rule in `CLAUDE.md` says the only crates allowed
    /// to touch raw Vulkan types are the RHI itself and the in-tree
    /// adapters. Mirror of [`crate::core::rhi::StreamTexture::vulkan_inner`].
    #[cfg(target_os = "linux")]
    pub fn vulkan_inner(&self) -> &std::sync::Arc<crate::vulkan::rhi::HostVulkanPixelBuffer> {
        &self.inner
    }

    /// Create an RhiPixelBufferRef from a raw IOSurfaceRef (macOS only).
    ///
    /// This is useful for cross-process frame sharing where the IOSurfaceRef
    /// is received from another process.
    ///
    /// # Safety
    /// The caller must ensure the IOSurfaceRef is valid.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub unsafe fn from_iosurface_ref(
        iosurface: crate::apple::corevideo_ffi::IOSurfaceRef,
    ) -> crate::core::Result<Self> {
        crate::metal::rhi::pixel_buffer_ref::from_iosurface_ref_impl(iosurface)
    }
}

impl Clone for RhiPixelBufferRef {
    fn clone(&self) -> Self {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::clone_impl(self)
        }
        #[cfg(target_os = "linux")]
        {
            Self {
                inner: std::sync::Arc::clone(&self.inner),
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Self {
                _marker: std::marker::PhantomData,
            }
        }
    }
}

impl Drop for RhiPixelBufferRef {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::drop_impl(self);
        }
        // On Linux, HostVulkanPixelBuffer handles its own cleanup via its Drop impl.
        // No additional action needed here.
    }
}

// Safety: CVPixelBufferRef is thread-safe (reference counted with atomic ops)
unsafe impl Send for RhiPixelBufferRef {}
unsafe impl Sync for RhiPixelBufferRef {}

impl std::fmt::Debug for RhiPixelBufferRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiPixelBufferRef")
            .field("format", &self.format())
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}
