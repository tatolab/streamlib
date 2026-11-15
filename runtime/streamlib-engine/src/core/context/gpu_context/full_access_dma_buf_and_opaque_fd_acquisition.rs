// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContextFullAccess {
    /// Construct an OPAQUE_FD-exportable timeline semaphore — the
    /// FullAccess-callable entry point over
    /// [`GpuContext::create_exportable_timeline_semaphore`].
    pub fn create_exportable_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        self.host_inner()
            .create_exportable_timeline_semaphore(initial_value)
    }

    /// Allocate a render-target-capable DMA-BUF VkImage (privileged path —
    /// host-only adapter primitive, customers never see this directly).
    /// See [`GpuContext::acquire_render_target_dma_buf_image`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_render_target_dma_buf_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<OneOffAcquisitionCountedForItsNode<Texture>> {
        acquire_a_one_off_counted_against_its_node(
            self.one_off_acquisitions_of_the_node.as_ref(),
            OneOffAcquisitionShape::RenderTargetImage {
                width,
                height,
                format,
            },
            || {
                self.host_inner()
                    .acquire_render_target_dma_buf_image(width, height, format)
            },
        )
    }

    /// Import a DMA-BUF FD as a `StorageBuffer` handle. Camera V4L2
    /// zero-copy path. Consumes `dma_buf_fd` whatever the outcome: the
    /// driver owns it behind a successful import and a failure closes it.
    pub fn import_dma_buf_storage_buffer(
        &self,
        dma_buf_fd: std::os::fd::OwnedFd,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.host_inner()
            .import_dma_buf_storage_buffer(dma_buf_fd, byte_size)
    }

    /// Export a fresh dup'd DMA-BUF FD + byte size for a `PixelBuffer`.
    /// The fd transfers to the caller.
    ///
    /// A helper process reaching
    /// for a DMA-BUF handle goes through surface-share, which already
    /// carries the fd over `SCM_RIGHTS`.
    pub fn export_pixel_buffer_dma_buf_fd(
        &self,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64)> {
        self.host_inner()
            .export_pixel_buffer_dma_buf_fd(pixel_buffer)
    }

    /// Allocate an OPAQUE_FD-exportable `VkBuffer` as a `StorageBuffer`
    /// (`device_local` picks VRAM-resident vs HOST_VISIBLE). The
    /// OPAQUE_FD/CUDA producer allocation (#1262).
    pub fn create_opaque_fd_export_buffer(
        &self,
        byte_size: u64,
        device_local: bool,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.host_inner()
            .create_opaque_fd_export_buffer(byte_size, device_local)
    }

    /// Export a fresh dup'd OPAQUE_FD + byte size + exporting-device UUID
    /// from a `StorageBuffer`. The fd transfers to the caller.
    pub fn export_storage_buffer_opaque_fd(
        &self,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        self.host_inner().export_storage_buffer_opaque_fd(buffer)
    }

    /// Wrap an OPAQUE_FD `StorageBuffer` as a `PixelBuffer` sharing the
    /// same allocation so it can register through the surface-store
    /// `register_pixel_buffer_with_timeline` path (#1262).
    pub fn wrap_storage_buffer_as_pixel_buffer(
        &self,
        storage_buffer: &crate::core::rhi::StorageBuffer,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: crate::core::rhi::PixelFormat,
    ) -> Result<crate::core::rhi::PixelBuffer> {
        self.host_inner().wrap_storage_buffer_as_pixel_buffer(
            storage_buffer,
            width,
            height,
            bytes_per_pixel,
            format,
        )
    }

    /// Per-frame CUDA producer copy: image→buffer in one host-device
    /// submission with optional `consume_done` wait + `produce_done`
    /// signal (#1262).
    pub fn copy_texture_to_storage_buffer_and_signal(
        &self,
        source_texture: &crate::core::rhi::Texture,
        source_layout: crate::core::rhi::VulkanLayout,
        dst: &crate::core::rhi::StorageBuffer,
        consume_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
        produce_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
    ) -> Result<()> {
        self.host_inner().copy_texture_to_storage_buffer_and_signal(
            source_texture,
            source_layout,
            dst,
            consume_done,
            produce_done,
        )
    }
}
