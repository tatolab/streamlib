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

    /// Linux: OPAQUE_FD file descriptor for Vulkan-aware importers.
    ///
    /// Used for cross-process Vulkan memory sharing where the importer is
    /// also Vulkan-aware (CUDA via UUID-matched device, OpenCL, another
    /// VkInstance) and tile-aware DRM-modifier negotiation isn't needed.
    /// Must be passed via SCM_RIGHTS ancillary data.
    ///
    /// Source-side allocation is via
    /// [`crate::vulkan::rhi::HostVulkanPixelBuffer::new_opaque_fd_export`].
    /// Consumer-side import is via
    /// `streamlib_consumer_rhi::ConsumerVulkanPixelBuffer::from_opaque_fd`
    /// or, in CUDA's case, `cudaImportExternalMemory` with
    /// `cudaExternalMemoryHandleTypeOpaqueFd` directly.
    #[cfg(target_os = "linux")]
    OpaqueFd {
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

    /// Export one handle per plane for multi-plane DMA-BUFs. The default
    /// implementation wraps [`Self::export_handle`] in a single-element vec
    /// — correct for every single-allocation format in tree today (BGRA,
    /// RGBA, NV12 contiguous). Backends that truly split planes across
    /// separate allocations (e.g. NV12 under `VK_EXT_image_drm_format_modifier`
    /// with disjoint Y and UV) must override.
    fn export_plane_handles(&self) -> Result<Vec<RhiExternalHandle>> {
        Ok(vec![self.export_handle()?])
    }
}

/// Extension trait for importing RhiPixelBuffer from external handle.
pub trait RhiPixelBufferImport {
    /// Import a GPU buffer from a single external handle.
    fn from_external_handle(
        handle: RhiExternalHandle,
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self>
    where
        Self: Sized;

    /// Import a multi-plane GPU buffer from one external handle per plane.
    ///
    /// The default implementation only accepts a single-plane input —
    /// backends that can't natively represent multiple planes still
    /// compile and refuse multi-plane input at runtime. Linux overrides
    /// with a real multi-plane import so the Rust surface-store path
    /// keeps feature parity with the polyglot Python and Deno shims.
    fn from_external_plane_handles(
        handles: &[RhiExternalHandle],
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        match handles {
            [only] => Self::from_external_handle(only.clone(), width, height, format),
            [] => Err(crate::core::StreamError::Configuration(
                "from_external_plane_handles: empty plane vec".into(),
            )),
            _ => Err(crate::core::StreamError::NotSupported(
                "multi-plane import is only implemented on Linux today".into(),
            )),
        }
    }
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
        Self::from_external_plane_handles(&[handle], width, height, format)
    }

    fn from_external_plane_handles(
        handles: &[RhiExternalHandle],
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self> {
        if handles.is_empty() {
            return Err(crate::core::StreamError::Configuration(
                "DMA-BUF import: empty plane vec".into(),
            ));
        }

        // Reject any non-DMA-BUF handle types up front, before we touch the
        // global Vulkan device or pixel-format machinery — so the contract
        // is unit-testable without a live `HostVulkanDevice`.
        for h in handles {
            if let RhiExternalHandle::OpaqueFd { .. } = h {
                return Err(crate::core::StreamError::NotSupported(
                    "RhiPixelBufferImport::from_external_plane_handles: \
                     OPAQUE_FD handles must be imported via \
                     ConsumerVulkanPixelBuffer::from_opaque_fd, not \
                     this host-side DMA-BUF constructor"
                        .into(),
                ));
            }
        }

        let vulkan_device =
            crate::vulkan::rhi::vulkan_pixel_buffer::VULKAN_DEVICE_FOR_IMPORT
                .get()
                .ok_or_else(|| {
                    crate::core::StreamError::NotSupported(
                        "DMA-BUF import: HostVulkanDevice not initialized (GpuDevice::new() not called)"
                            .into(),
                    )
                })?;

        let bytes_per_pixel = format.bits_per_pixel() / 8;
        if bytes_per_pixel == 0 {
            return Err(crate::core::StreamError::Configuration(
                "DMA-BUF import: unsupported pixel format (0 bits per pixel)".into(),
            ));
        }

        // Unpack every plane's fd + size. OPAQUE_FD has already been
        // rejected above; only `DmaBuf` reaches this loop on Linux.
        let mut fds: Vec<std::os::unix::io::RawFd> = Vec::with_capacity(handles.len());
        let mut plane_sizes: Vec<vulkanalia::vk::DeviceSize> = Vec::with_capacity(handles.len());
        for (idx, h) in handles.iter().enumerate() {
            let (fd, size) = match h.clone() {
                RhiExternalHandle::DmaBuf { fd, size } => (fd, size),
                // Unreachable: rejected up-front above; kept for
                // exhaustiveness so future variants force a compile error.
                RhiExternalHandle::OpaqueFd { .. } => unreachable!(
                    "OPAQUE_FD handle should have been rejected by the up-front \
                     handle-type validation"
                ),
            };
            fds.push(fd);
            let effective = if size > 0 {
                size as vulkanalia::vk::DeviceSize
            } else if idx == 0 && width > 0 && height > 0 {
                // Plane 0 falls back to width*height*bpp (back-compat with
                // legacy single-plane callers that don't pass a size).
                (width as u64) * (height as u64) * (bytes_per_pixel as u64)
            } else {
                return Err(crate::core::StreamError::Configuration(format!(
                    "DMA-BUF import: plane {} has size=0 and cannot be derived",
                    idx
                )));
            };
            plane_sizes.push(effective);
        }

        let vulkan_pixel_buffer =
            crate::vulkan::rhi::HostVulkanPixelBuffer::from_dma_buf_fds(
                vulkan_device,
                &fds,
                &plane_sizes,
                width,
                height,
                bytes_per_pixel,
                format,
            )?;

        let pixel_buffer_ref = super::RhiPixelBufferRef {
            inner: std::sync::Arc::new(vulkan_pixel_buffer),
        };

        Ok(super::RhiPixelBuffer::new(pixel_buffer_ref))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn opaque_fd_and_dma_buf_with_same_fields_are_not_equal() {
        // Variant discriminant must distinguish OPAQUE_FD vs DMA-BUF even
        // when fd + size are byte-identical — the consumer side dispatches
        // on the variant, not the fields.
        let dma = RhiExternalHandle::DmaBuf { fd: 42, size: 4096 };
        let opaque = RhiExternalHandle::OpaqueFd { fd: 42, size: 4096 };
        assert_ne!(dma, opaque);
    }

    #[test]
    fn opaque_fd_debug_includes_variant_name() {
        // Tracing relies on Debug to disambiguate handle types in logs.
        let opaque = RhiExternalHandle::OpaqueFd { fd: 7, size: 128 };
        let s = format!("{opaque:?}");
        assert!(s.contains("OpaqueFd"), "got: {s}");
        assert!(s.contains("fd: 7"), "got: {s}");
        assert!(s.contains("size: 128"), "got: {s}");
    }

    #[test]
    fn host_side_dma_buf_constructor_rejects_opaque_fd_handles() {
        // Contract: `RhiPixelBufferImport::from_external_plane_handles`
        // builds a `HostVulkanPixelBuffer` from DMA-BUF FDs only. OPAQUE_FD
        // handles take a different path (`ConsumerVulkanPixelBuffer::from_opaque_fd`)
        // and must be rejected up-front with a clear error rather than
        // silently miscoercing through the DMA-BUF code path.
        let opaque = RhiExternalHandle::OpaqueFd { fd: -1, size: 0 };
        let result =
            <super::super::RhiPixelBuffer as RhiPixelBufferImport>::from_external_plane_handles(
                &[opaque],
                1,
                1,
                super::super::PixelFormat::Bgra32,
            );
        match result {
            Err(crate::core::StreamError::NotSupported(msg)) => {
                assert!(
                    msg.contains("OPAQUE_FD"),
                    "error message must mention OPAQUE_FD: {msg}"
                );
                assert!(
                    msg.contains("ConsumerVulkanPixelBuffer::from_opaque_fd"),
                    "error must point at the right alternative: {msg}"
                );
            }
            other => panic!("expected NotSupported, got: {other:?}"),
        }
    }
}
