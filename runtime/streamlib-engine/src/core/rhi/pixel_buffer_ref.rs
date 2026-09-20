// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform pixel buffer reference.

use super::PixelFormat;

/// Platform pixel buffer reference.
///
/// Holds an `Arc<HostVulkanBuffer>` — the role-agnostic bottom-layer
/// primitive, which carries no pixel shape — plus the pixel-shaped metadata
/// this reference adds: width, height, format, bytes-per-pixel.
///
/// Clone bumps the Arc's refcount; no image data is ever copied.
#[derive(Clone)]
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

impl std::fmt::Debug for PixelBufferRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBufferRef")
            .field("format", &self.format())
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}
