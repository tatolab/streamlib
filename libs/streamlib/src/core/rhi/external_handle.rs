// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI external handle for cross-process GPU resource sharing.

use crate::core::Result;

/// Platform-agnostic GPU resource handle for cross-process sharing.
///
/// This enum represents a handle that can be sent to another process,
/// which can then import the GPU resource without copying data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RhiExternalHandle {
    /// macOS: IOSurface ID (u32).
    /// Can be looked up in another process via IOSurfaceLookup().
    /// Note: This only works with kIOSurfaceIsGlobal (deprecated/removed).
    #[cfg(target_os = "macos")]
    IOSurface { id: u32 },

    /// macOS: IOSurface via mach port for cross-process sharing.
    /// The mach port is created via IOSurfaceCreateMachPort().
    #[cfg(target_os = "macos")]
    IOSurfaceMachPort { port: u32 },

    /// Linux: DMA-BUF file descriptor.
    /// Must be passed via SCM_RIGHTS ancillary data.
    #[cfg(target_os = "linux")]
    DmaBuf {
        fd: std::os::unix::io::RawFd,
        size: usize,
    },

    /// Windows: DXGI shared handle.
    /// Can be opened in another process via OpenSharedHandle().
    #[cfg(target_os = "windows")]
    DxgiShared { handle: *mut std::ffi::c_void },
}

// SAFETY: RhiExternalHandle is Send because it contains only handles/IDs
// that can be safely sent between threads.
unsafe impl Send for RhiExternalHandle {}
unsafe impl Sync for RhiExternalHandle {}

impl RhiExternalHandle {
    /// Extract the mach port from an IOSurfaceMachPort handle (macOS only).
    #[cfg(target_os = "macos")]
    pub fn mach_port(&self) -> Option<u32> {
        match self {
            RhiExternalHandle::IOSurfaceMachPort { port } => Some(*port),
            _ => None,
        }
    }
}

/// Extension trait for exporting RhiPixelBuffer to external handle.
pub trait RhiPixelBufferExport {
    /// Export the GPU buffer for sharing with another process.
    fn export_handle(&self) -> Result<RhiExternalHandle>;
}

/// Extension trait for importing RhiPixelBuffer from external handle.
pub trait RhiPixelBufferImport {
    /// Import a GPU buffer from an external handle.
    fn from_external_handle(
        handle: RhiExternalHandle,
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self>
    where
        Self: Sized;
}

#[cfg(target_os = "linux")]
impl RhiPixelBufferExport for super::RhiPixelBuffer {
    fn export_handle(&self) -> Result<RhiExternalHandle> {
        let fd = self.ref_.inner.export_dma_buf_fd()?;
        let size = self.ref_.inner.size() as usize;
        Ok(RhiExternalHandle::DmaBuf { fd, size })
    }
}

#[cfg(target_os = "linux")]
impl RhiPixelBufferImport for super::RhiPixelBuffer {
    fn from_external_handle(
        handle: RhiExternalHandle,
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self> {
        let RhiExternalHandle::DmaBuf { fd, size } = handle;

        let vulkan_device =
            crate::vulkan::rhi::vulkan_pixel_buffer::VULKAN_DEVICE_FOR_IMPORT
                .get()
                .ok_or_else(|| {
                    crate::core::StreamError::NotSupported(
                        "DMA-BUF import: VulkanDevice not initialized (GpuDevice::new() not called)"
                            .into(),
                    )
                })?;

        let bytes_per_pixel = format.bits_per_pixel() / 8;
        let bytes_per_pixel = if bytes_per_pixel > 0 { bytes_per_pixel } else { 4 };

        let allocation_size = if size > 0 {
            size as u64
        } else if width > 0 && height > 0 {
            (width as u64) * (height as u64) * (bytes_per_pixel as u64)
        } else {
            return Err(crate::core::StreamError::Configuration(
                "DMA-BUF import: cannot determine allocation size (size=0, width=0 or height=0)"
                    .into(),
            ));
        };

        let vulkan_pixel_buffer =
            crate::vulkan::rhi::VulkanPixelBuffer::from_dma_buf_fd(
                vulkan_device,
                fd,
                width,
                height,
                bytes_per_pixel,
                format,
                allocation_size,
            )?;

        let pixel_buffer_ref = super::RhiPixelBufferRef {
            inner: std::sync::Arc::new(vulkan_pixel_buffer),
        };

        Ok(super::RhiPixelBuffer::new(pixel_buffer_ref))
    }
}
