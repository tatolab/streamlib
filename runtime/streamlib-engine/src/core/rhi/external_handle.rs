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
    /// [`crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export`].
    /// Consumer-side import is via
    /// `streamlib_consumer_rhi::ConsumerVulkanBuffer::from_opaque_fd`
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

    /// The plane size the exporter stated for this fd (Linux only).
    #[cfg(target_os = "linux")]
    pub fn stated_size(&self) -> usize {
        match self {
            RhiExternalHandle::DmaBuf { size, .. } | RhiExternalHandle::OpaqueFd { size, .. } => {
                *size
            }
        }
    }
}

/// Extension trait for exporting PixelBuffer to external handle.
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

/// Extension trait for importing PixelBuffer from external handle.
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
    /// Takes the handles by value because on Linux the fd inside each one
    /// is consumed: handed to the driver at its import or closed before
    /// that, so the caller holds nothing after the call whatever its
    /// outcome.
    ///
    /// The default implementation only accepts a single-plane input —
    /// backends that can't natively represent multiple planes still
    /// compile and refuse multi-plane input at runtime. Linux overrides
    /// with a real multi-plane import so the Rust surface-store path
    /// keeps feature parity with the polyglot Python and Deno shims.
    fn from_external_plane_handles(
        handles: Vec<RhiExternalHandle>,
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        let mut handles = handles.into_iter();
        match (handles.next(), handles.next()) {
            (Some(only), None) => Self::from_external_handle(only, width, height, format),
            (None, _) => Err(crate::core::Error::Configuration(
                "from_external_plane_handles: empty plane vec".into(),
            )),
            _ => Err(crate::core::Error::NotSupported(
                "multi-plane import is only implemented on Linux today".into(),
            )),
        }
    }
}

#[cfg(target_os = "linux")]
impl RhiPixelBufferExport for super::PixelBuffer {
    /// Returns the natural handle type for the underlying allocation —
    /// `RhiExternalHandle::OpaqueFd` for OPAQUE_FD-flavored buffers
    /// (see [`crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export`]),
    /// `RhiExternalHandle::DmaBuf` otherwise. Callers dispatch on the
    /// returned variant.
    fn export_handle(&self) -> Result<RhiExternalHandle> {
        self.buffer_ref().inner.export_external_handle()
    }
}

#[cfg(target_os = "linux")]
impl RhiPixelBufferImport for super::PixelBuffer {
    fn from_external_handle(
        handle: RhiExternalHandle,
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self> {
        Self::from_external_plane_handles(vec![handle], width, height, format)
    }

    /// Import one DMA-BUF fd per plane; see the trait for the fd contract.
    fn from_external_plane_handles(
        handles: Vec<RhiExternalHandle>,
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self> {
        use std::os::fd::{FromRawFd as _, OwnedFd};

        // Adopted before anything is validated, so a refusal closes every
        // fd rather than only the ones a loop reached.
        let mut plane_fds: Vec<OwnedFd> = Vec::with_capacity(handles.len());
        let mut handles_include_an_opaque_fd_plane = false;
        for handle in &handles {
            let fd = match *handle {
                RhiExternalHandle::DmaBuf { fd, .. } => fd,
                RhiExternalHandle::OpaqueFd { fd, .. } => {
                    handles_include_an_opaque_fd_plane = true;
                    fd
                }
            };
            // SAFETY: the caller surrendered this fd to the import and holds
            // no other owner of it.
            plane_fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
        }

        if plane_fds.is_empty() {
            return Err(crate::core::Error::Configuration(
                "DMA-BUF import: empty plane vec".into(),
            ));
        }

        // OPAQUE_FD is refused before the global Vulkan device or the
        // pixel-format machinery is touched, so the contract is
        // unit-testable without a live `HostVulkanDevice`.
        if handles_include_an_opaque_fd_plane {
            return Err(crate::core::Error::NotSupported(
                "RhiPixelBufferImport::from_external_plane_handles: \
                 OPAQUE_FD handles must be imported via \
                 ConsumerVulkanBuffer::from_opaque_fd, not \
                 this host-side DMA-BUF constructor"
                    .into(),
            ));
        }

        let vulkan_device =
            crate::vulkan::rhi::vulkan_buffer::VULKAN_DEVICE_FOR_IMPORT
                .get()
                .ok_or_else(|| {
                    crate::core::Error::NotSupported(
                        "DMA-BUF import: HostVulkanDevice not initialized (GpuDevice::new() not called)"
                            .into(),
                    )
                })?;

        let bytes_per_pixel = format.bits_per_pixel() / 8;
        if bytes_per_pixel == 0 {
            return Err(crate::core::Error::Configuration(
                "DMA-BUF import: unsupported pixel format (0 bits per pixel)".into(),
            ));
        }

        let mut plane_sizes: Vec<vulkanalia::vk::DeviceSize> = Vec::with_capacity(handles.len());
        for (idx, handle) in handles.iter().enumerate() {
            let size = handle.stated_size();
            let effective = if size > 0 {
                size as vulkanalia::vk::DeviceSize
            } else if idx == 0 && width > 0 && height > 0 {
                // Plane 0 falls back to width*height*bpp (back-compat with
                // legacy single-plane callers that don't pass a size).
                (width as u64) * (height as u64) * (bytes_per_pixel as u64)
            } else {
                return Err(crate::core::Error::Configuration(format!(
                    "DMA-BUF import: plane {} has size=0 and cannot be derived",
                    idx
                )));
            };
            plane_sizes.push(effective);
        }

        let vulkan_buffer = crate::vulkan::rhi::HostVulkanBuffer::from_dma_buf_fds(
            vulkan_device,
            plane_fds,
            &plane_sizes,
        )?;

        let pixel_buffer_ref = super::PixelBufferRef {
            inner: std::sync::Arc::new(vulkan_buffer),
            width,
            height,
            bytes_per_pixel,
            format,
        };

        Ok(super::PixelBuffer::new(pixel_buffer_ref))
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

    /// A pipe stands in for a plane fd: each pipe has its own inode, and
    /// holding the write end keeps that inode alive after the read end
    /// closes, so "was it closed?" is answered without racing a parallel
    /// test thread for the recycled number.
    struct PlaneFdUnderTest {
        plane_fd: std::os::unix::io::RawFd,
        write_end_fd: std::os::unix::io::RawFd,
        inode: u64,
    }

    fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
        let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
            return None;
        }
        Some(unsafe { file_status.assume_init() }.st_ino as u64)
    }

    impl PlaneFdUnderTest {
        fn mint() -> Self {
            let mut pipe_ends = [0 as std::os::unix::io::RawFd; 2];
            assert_eq!(
                unsafe { libc::pipe2(pipe_ends.as_mut_ptr(), libc::O_CLOEXEC) },
                0
            );
            let inode = inode_of(pipe_ends[0]).expect("a fresh pipe must stat");
            Self {
                plane_fd: pipe_ends[0],
                write_end_fd: pipe_ends[1],
                inode,
            }
        }

        fn was_closed(&self) -> bool {
            inode_of(self.plane_fd) != Some(self.inode)
        }
    }

    impl Drop for PlaneFdUnderTest {
        fn drop(&mut self) {
            unsafe {
                if !self.was_closed() {
                    libc::close(self.plane_fd);
                }
                libc::close(self.write_end_fd);
            }
        }
    }

    /// OPAQUE_FD takes a different path (`ConsumerVulkanBuffer::from_opaque_fd`)
    /// and is refused up-front rather than miscoerced through the DMA-BUF
    /// import — and the refusal owns what it was handed: every plane fd,
    /// the DMA-BUF one included, is closed rather than left to no one.
    #[test]
    fn a_refused_opaque_fd_import_closes_every_plane_fd_it_was_handed() {
        let dma_buf_plane = PlaneFdUnderTest::mint();
        let opaque_plane = PlaneFdUnderTest::mint();
        let result =
            <super::super::PixelBuffer as RhiPixelBufferImport>::from_external_plane_handles(
                vec![
                    RhiExternalHandle::DmaBuf {
                        fd: dma_buf_plane.plane_fd,
                        size: 4096,
                    },
                    RhiExternalHandle::OpaqueFd {
                        fd: opaque_plane.plane_fd,
                        size: 4096,
                    },
                ],
                1,
                1,
                super::super::PixelFormat::Bgra32,
            );
        match result {
            Err(crate::core::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("OPAQUE_FD"),
                    "error message must mention OPAQUE_FD: {msg}"
                );
                assert!(
                    msg.contains("ConsumerVulkanBuffer::from_opaque_fd"),
                    "error must point at the right alternative: {msg}"
                );
            }
            other => panic!("expected NotSupported, got: {other:?}"),
        }
        assert!(
            dma_buf_plane.was_closed(),
            "the refusal left the DMA-BUF plane fd open — no owner remains to close it"
        );
        assert!(
            opaque_plane.was_closed(),
            "the refusal left the OPAQUE_FD plane fd open — no owner remains to close it"
        );
    }
}
