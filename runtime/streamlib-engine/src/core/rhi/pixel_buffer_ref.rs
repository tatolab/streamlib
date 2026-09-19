// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform pixel buffer reference.

use super::PixelFormat;

/// Platform pixel buffer reference.
///
/// Wraps the platform's native pixel buffer type plus its pixel-shaped
/// metadata (width, height, format, bytes-per-pixel):
/// - Linux / macOS: `Arc<HostVulkanBuffer>` (shared generic-buffer primitive)
///   plus pixel metadata stored on this reference (the bottom-layer
///   `HostVulkanBuffer` is role-agnostic and does not carry pixel shape).
/// - Windows: `ID3D11Texture2D*` (future)
///
/// Clone increments the appropriate refcount, Drop decrements it.
/// No image data is ever copied.
pub struct PixelBufferRef {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) inner: std::sync::Arc<crate::vulkan::rhi::HostVulkanBuffer>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) width: u32,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) height: u32,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) bytes_per_pixel: u32,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) format: PixelFormat,

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) _marker: std::marker::PhantomData<()>,
}

impl PixelBufferRef {
    /// Query the pixel format.
    pub fn format(&self) -> PixelFormat {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.format
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            PixelFormat::Unknown
        }
    }

    /// Query the width.
    pub fn width(&self) -> u32 {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.width
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            0
        }
    }

    /// Query the height.
    pub fn height(&self) -> u32 {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.height
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            0
        }
    }

    /// Bytes per pixel.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn bytes_per_pixel(&self) -> u32 {
        self.bytes_per_pixel
    }

    /// Number of DMA-BUF planes backing this pixel buffer.
    ///
    /// `1` for VMA-allocated buffers and single-plane DMA-BUF imports.
    /// `N` for multi-plane imports — mirror of
    /// `slpn_gpu_surface_plane_count` / `sldn_gpu_surface_plane_count`
    /// on the polyglot shim side. On an unsupported platform this always
    /// reports `1`.
    pub fn plane_count(&self) -> u32 {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.inner.plane_count()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            1
        }
    }

    /// Mapped base address for `plane_index`, or null if the plane index
    /// is out of range or the backend doesn't expose CPU-mapped planes.
    pub fn plane_base_address(&self, plane_index: u32) -> *mut u8 {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.inner.plane_mapped_ptr(plane_index)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = plane_index;
            std::ptr::null_mut()
        }
    }

    /// Byte size of `plane_index`, or `0` if the plane index is out of
    /// range.
    pub fn plane_size(&self, plane_index: u32) -> u64 {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.inner.plane_size(plane_index) as u64
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = plane_index;
            0
        }
    }
}

impl Clone for PixelBufferRef {
    fn clone(&self) -> Self {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            Self {
                inner: std::sync::Arc::clone(&self.inner),
                width: self.width,
                height: self.height,
                bytes_per_pixel: self.bytes_per_pixel,
                format: self.format,
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Self {
                _marker: std::marker::PhantomData,
            }
        }
    }
}

// Safety: the backing `HostVulkanBuffer` is itself Send+Sync.
unsafe impl Send for PixelBufferRef {}
unsafe impl Sync for PixelBufferRef {}

impl std::fmt::Debug for PixelBufferRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBufferRef")
            .field("format", &self.format())
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}
