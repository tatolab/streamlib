// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Construct an OPAQUE_FD-exportable timeline semaphore against the
    /// host's vulkan device. Backs
    /// [`GpuContextFullAccess::create_exportable_timeline_semaphore`]
    /// which is the FullAccess-callable entry point.
    ///
    /// The returned semaphore is created with
    /// `VK_KHR_external_semaphore_fd` export support so its
    /// `export_opaque_fd` can hand a fresh OPAQUE_FD to a subprocess
    /// consumer (surface-share / CUDA cross-process sync).
    pub fn create_exportable_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        let device = self.device.inner.device();
        let sem =
            crate::vulkan::rhi::HostVulkanTimelineSemaphore::new_exportable(device, initial_value)?;
        Ok(Arc::new(sem))
    }

    /// Allocate a render-target-capable DMA-BUF VkImage backed by the device's
    /// tiled-modifier VMA pool.
    ///
    /// The driver picks one of the EGL-advertised render-target modifiers
    /// from [`HostVulkanDevice::drm_modifier_table`]. The resulting
    /// `Texture` carries the chosen modifier on its inner
    /// [`HostVulkanTexture`] (see [`HostVulkanTexture::chosen_drm_format_modifier`]),
    /// ready to be carried in a `SurfaceTransportHandle` when the host
    /// surface-share service registers the surface.
    ///
    /// Errors when the EGL probe didn't find an RT-capable modifier for
    /// `format` — there is no silent fallback to LINEAR (sampler-only on
    /// NVIDIA).
    ///
    /// Picking the right acquire method:
    /// - **In-process texture for sampling/compute**: use
    ///   [`Self::acquire_texture`] (`VK_IMAGE_TILING_OPTIMAL`, no
    ///   DMA-BUF export pressure).
    /// - **CPU-readable buffer (mmap/PNG sample/MMAP fallback)**: use
    ///   [`Self::acquire_pixel_buffer`] (`VkBuffer`, linear).
    /// - **Cross-process surface a consumer adapter renders into**:
    ///   this method (tiled DRM modifier, DMA-BUF exportable, FBO-completable
    ///   on the consumer side).
    pub fn acquire_render_target_dma_buf_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<Texture> {
        tracing::debug!(
            rhi_op = "acquire_render_target_dma_buf_image",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_render_target_dma_buf_image"
        );

        let desc = TextureDescriptor::new(width, height, format)
            .with_usage(render_target_texture_usages());
        self.device.create_texture_render_target_dma_buf(&desc)
    }

    /// Import the first `readable_byte_len` bytes of a caller-owned mapped
    /// range for GPU reads, taking the imported tier when the driver allows
    /// it and host-visible staging otherwise. See
    /// [`HostMappingReadByGpu`](crate::vulkan::rhi::HostMappingReadByGpu) for
    /// the per-read protocol.
    ///
    /// # Safety
    ///
    /// All `mapped_byte_len` bytes must stay mapped and readable until the
    /// returned value drops.
    pub(crate) unsafe fn import_host_mapping_for_gpu_reads(
        &self,
        host_range_ptr: *const u8,
        mapped_byte_len: usize,
        readable_byte_len: usize,
    ) -> Result<crate::vulkan::rhi::HostMappingReadByGpu> {
        // SAFETY: the caller upholds this method's own contract.
        unsafe {
            crate::vulkan::rhi::HostMappingReadByGpu::import_for_gpu_reads(
                &self.device.inner,
                host_range_ptr,
                mapped_byte_len,
                readable_byte_len,
            )
        }
    }

    /// Give the first `readable_byte_len` bytes of a caller-owned mapped
    /// range to GPU reads through host-visible staging, never asking the
    /// driver to import it. See
    /// [`HostMappingReadByGpu::stage_for_gpu_reads`](crate::vulkan::rhi::HostMappingReadByGpu::stage_for_gpu_reads).
    ///
    /// # Safety
    ///
    /// All `readable_byte_len` bytes must stay mapped and readable until the
    /// returned value drops.
    pub(crate) unsafe fn stage_host_mapping_for_gpu_reads(
        &self,
        host_range_ptr: *const u8,
        readable_byte_len: usize,
        why_the_range_is_not_imported: String,
    ) -> Result<crate::vulkan::rhi::HostMappingReadByGpu> {
        // SAFETY: the caller upholds this method's own contract.
        unsafe {
            crate::vulkan::rhi::HostMappingReadByGpu::stage_for_gpu_reads(
                &self.device.inner,
                host_range_ptr,
                readable_byte_len,
                why_the_range_is_not_imported,
            )
        }
    }

    /// Import a DMA-BUF FD as a `StorageBuffer`. Camera V4L2 zero-copy
    /// path. Consumes `dma_buf_fd` whatever the outcome: the driver owns
    /// it behind a successful import and a failure closes it.
    pub fn import_dma_buf_storage_buffer(
        &self,
        dma_buf_fd: std::os::fd::OwnedFd,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        let vulkan_device = &self.device.inner;
        let buf = crate::vulkan::rhi::HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(
            vulkan_device,
            dma_buf_fd,
            byte_size,
        )?;
        Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
            Arc::new(buf),
        ))
    }

    /// Export a fresh dup'd DMA-BUF FD for `pixel_buffer`, plus its byte
    /// size. Ownership of the fd transfers to the caller.
    ///
    /// The counterpart of [`Self::import_dma_buf_storage_buffer`], and the
    /// export half of the handle-shaped native-interop surface: external
    /// code that speaks DMA-BUF (EGL, a V4L2 output device, another
    /// process) receives the frame in its own dialect without the engine
    /// exposing any Vulkan.
    ///
    /// Only a DMA-BUF-flavoured allocation can answer — pixel-buffer pool
    /// buffers on a device that exports DMA-BUF. An OPAQUE_FD allocation,
    /// which is what a pool buffer is on a device that does not, has no
    /// DMA-BUF export path, so this is refused by name rather than returning
    /// a handle the importer cannot use.
    pub fn export_pixel_buffer_dma_buf_fd(
        &self,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64)> {
        use crate::host_rhi::HostPixelBufferRefExt as _;
        let host_buffer = pixel_buffer.buffer_ref().vulkan_inner();
        let fd = host_buffer.export_dma_buf_fd()?;
        Ok((fd, host_buffer.size()))
    }

    /// Allocate an OPAQUE_FD-exportable `VkBuffer` as a `StorageBuffer`.
    /// `device_local = true` picks the VRAM-resident CUDA-visible pool
    /// (`new_opaque_fd_export_device_local`); `false` picks the
    /// HOST_VISIBLE pool (`new_opaque_fd_export`). Backs
    /// [`GpuContextFullAccess::create_opaque_fd_export_buffer`], the
    /// OPAQUE_FD/CUDA producer allocation (#1262).
    pub fn create_opaque_fd_export_buffer(
        &self,
        byte_size: u64,
        device_local: bool,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        let vulkan_device = &self.device.inner;
        let buf = if device_local {
            crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export_device_local(
                vulkan_device,
                byte_size,
            )?
        } else {
            crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export(vulkan_device, byte_size)?
        };
        Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
            Arc::new(buf),
        ))
    }

    /// Export a fresh dup'd OPAQUE_FD from `buffer` plus its byte size
    /// and the exporting device's `VkPhysicalDeviceIDProperties::deviceUUID`.
    /// The fd ownership transfers to the caller; the 16-byte UUID is the
    /// entire CUDA device-binding contract on multi-GPU rigs (a
    /// CUDA adapter matches the CUDA device whose `cudaDeviceProp::uuid`
    /// equals this value, never a silent fall-through to CUDA device 0).
    /// Backs [`GpuContextFullAccess::export_storage_buffer_opaque_fd`]
    /// (#1262).
    pub fn export_storage_buffer_opaque_fd(
        &self,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        let host_inner = buffer.host_inner();
        let fd = host_inner.export_opaque_fd_memory()?;
        let size = buffer.byte_size();
        // The UUID is the entire CUDA device-binding contract: it MUST
        // name the device that actually owns the exported memory, not the
        // `GpuContext`'s own device. Under the single-GPU invariant these
        // are the same, but sourcing from the buffer's owning
        // `HostVulkanDevice` keeps the binding correct if a `StorageBuffer`
        // ever originates from a different device (never a silent
        // mismatch).
        let uuid = host_inner.vulkan_device().physical_device_uuid();
        Ok((fd, size, uuid))
    }

    /// Wrap an existing OPAQUE_FD `StorageBuffer` (flat `VkBuffer`) as a
    /// `PixelBuffer` sharing the same `Arc<HostVulkanBuffer>`, so the flat
    /// CUDA buffer can register through the existing
    /// `SurfaceStore::register_pixel_buffer_with_timeline` path. Backs
    /// [`GpuContextFullAccess::wrap_storage_buffer_as_pixel_buffer`]
    /// (#1262).
    pub fn wrap_storage_buffer_as_pixel_buffer(
        &self,
        storage_buffer: &crate::core::rhi::StorageBuffer,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: crate::core::rhi::PixelFormat,
    ) -> Result<crate::core::rhi::PixelBuffer> {
        let inner = storage_buffer.host_inner_arc();
        Ok(crate::core::rhi::PixelBuffer::from_host_vulkan_buffer(
            inner,
            width,
            height,
            bytes_per_pixel,
            format,
        ))
    }

    /// Per-frame CUDA producer copy: in one host-device submission,
    /// optionally GPU-wait `consume_done` (`(timeline, wait_value)`),
    /// `vkCmdCopyImageToBuffer` from `source_texture` (currently in
    /// `source_layout`) into `dst`, then optionally signal `produce_done`
    /// (`(timeline, signal_value)`) on completion. When both timelines
    /// are `None` the submission blocks host-side via `submit_and_wait`.
    /// Backs
    /// [`GpuContextFullAccess::copy_texture_to_storage_buffer_and_signal`]
    /// (#1262).
    ///
    /// The source is copied directly from `source_layout` (the camera
    /// leaves ring textures in `GENERAL`, a legal copy-source layout), so
    /// no extra layout transition is recorded — the timeline signal's
    /// completion guarantee is what orders the buffer write ahead of the
    /// consumer's `acquire_read`.
    pub fn copy_texture_to_storage_buffer_and_signal(
        &self,
        source_texture: &crate::core::rhi::Texture,
        source_layout: crate::core::rhi::VulkanLayout,
        dst: &crate::core::rhi::StorageBuffer,
        consume_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
        produce_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
    ) -> Result<()> {
        let vulkan_device = &self.device.inner;
        let mut recorder = crate::vulkan::rhi::RhiCommandRecorderInner::new(
            vulkan_device,
            "copy_texture_to_storage_buffer_and_signal",
        )?;
        recorder.begin()?;
        let region = crate::vulkan::rhi::ImageCopyRegion::tightly_packed(
            source_texture.width(),
            source_texture.height(),
        );
        recorder.record_copy_image_to_buffer(source_texture, source_layout, dst, region)?;
        // Every branch must leave the shared-buffer write with a defined
        // completion contract before returning — the single-writer
        // producer copy is unsound otherwise (a consumer could read a
        // half-written buffer). Two contracts are valid: a `produce_done`
        // GPU signal that the consumer waits on (the async per-frame
        // path), or a host-side fence drain. Handle all four
        // (consume, produce) combinations explicitly so none returns
        // without one.
        match (consume_done, produce_done) {
            // No timelines: bare submit + host-side fence drain. The copy
            // is complete before return.
            (None, None) => recorder.submit_and_wait(),
            // A `produce_done` signal is present (with or without a
            // `consume_done` wait): the GPU-side signal IS the completion
            // contract — the consumer waits on `produce_done` before its
            // read — so the producer need not host-block. Covers
            // (None, Some) and (Some, Some).
            (consume, produce @ Some(_)) => {
                recorder.submit_waiting_and_signaling_timeline(consume, produce)
            }
            // A `consume_done` wait but no `produce_done` signal: the copy
            // GPU-waits on `consume_done`, but with no signal there is no
            // GPU-side ordering for the consumer. Host-block on the
            // recorder's completion fence so the shared-buffer write is
            // guaranteed done before return, keeping the single-writer
            // contract sound.
            (consume @ Some(_), None) => {
                recorder.submit_waiting_and_signaling_timeline(consume, None)?;
                recorder.wait_for_completion()
            }
        }
    }
}
