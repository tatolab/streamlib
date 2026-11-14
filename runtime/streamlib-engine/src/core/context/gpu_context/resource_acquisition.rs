// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Acquire a new output texture with a UUID, register it in the cache.
    pub fn acquire_output_texture(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<(String, Texture)> {
        let desc =
            TextureDescriptor::new(width, height, format).with_usage(output_texture_usages());
        let texture = self.device.create_texture(&desc)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.register_texture(&id, texture.clone());
        Ok((id, texture))
    }

    /// Acquire a pooled texture for in-process GPU work.
    ///
    /// Uses `VK_IMAGE_TILING_OPTIMAL` and is **not** safe to share with
    /// another process as a render target on NVIDIA Linux — the resulting
    /// DMA-BUF (if exported) is sampler-only there. For cross-process
    /// surfaces a consumer adapter will render INTO, use
    /// [`Self::acquire_render_target_dma_buf_image`] (Linux) instead.
    ///
    /// Never waits on another holder; past the device's memory budget the
    /// allocation is refused by name.
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        tracing::debug!(
            rhi_op = "acquire_texture",
            width = desc.width,
            height = desc.height,
            format = ?desc.format,
            "GpuContext::acquire_texture"
        );
        self.texture_pool
            .acquire(desc, self.loaded_stream_this_view_serves)
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
    #[cfg(target_os = "linux")]
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

    /// Allocate a render-target-capable VkImage over a fresh private
    /// IOSurface — the macOS peer of
    /// [`Self::acquire_render_target_dma_buf_image`], with the same usage
    /// set. Single-plane formats only.
    #[cfg(target_os = "macos")]
    pub fn acquire_render_target_iosurface_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<Texture> {
        tracing::debug!(
            rhi_op = "acquire_render_target_iosurface_image",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_render_target_iosurface_image"
        );
        let desc = TextureDescriptor::new(width, height, format)
            .with_usage(render_target_texture_usages());
        self.device.create_texture_iosurface_backed(&desc)
    }

    /// Acquire a caller-held storage buffer sized for `layout`: HOST_VISIBLE,
    /// persistently mapped, never pooled — the caller keeps the
    /// [`crate::core::rhi::StorageBuffer`] in its processor state and drops it
    /// at teardown. A byte-shaped SSBO is a one-dimensional `uint8` layout.
    ///
    /// Usage is `STORAGE_BUFFER | TRANSFER_SRC | TRANSFER_DST`, plus DMA-BUF
    /// export flags on Linux; compute kernels bind it via
    /// [`crate::vulkan::rhi::VulkanComputeKernel::set_storage_buffer_storage`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_storage_buffer(
        &self,
        layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.acquire_storage_buffer_of_flavour(
            layout,
            StorageBufferAllocationFlavour::CallerHeldHostVisible,
        )
    }

    /// Acquire a storage buffer for `layout` in the flavour its destination
    /// needs — derived by the engine per acquisition, never a caller's dial.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn acquire_storage_buffer_of_flavour(
        &self,
        layout: &crate::core::rhi::TensorStorageBufferLayout,
        flavour: StorageBufferAllocationFlavour,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        let byte_size = layout.byte_size();
        tracing::debug!(
            rhi_op = "acquire_storage_buffer",
            byte_size,
            ?flavour,
            "GpuContext::acquire_storage_buffer"
        );
        match flavour {
            StorageBufferAllocationFlavour::CallerHeldHostVisible => {
                let buffer = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
                    &self.device.inner,
                    byte_size,
                )?;
                Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
                    Arc::new(buffer),
                ))
            }
            #[cfg(target_os = "linux")]
            StorageBufferAllocationFlavour::CrossesToAHelperProcess => {
                self.create_opaque_fd_export_buffer(byte_size, true)
            }
            #[cfg(target_os = "macos")]
            StorageBufferAllocationFlavour::CrossesToAHelperProcess => {
                let buffer =
                    crate::vulkan::rhi::HostVulkanBuffer::new_iosurface_backed_storage_buffer(
                        &self.device.inner,
                        byte_size,
                    )?;
                Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
                    Arc::new(buffer),
                ))
            }
        }
    }

    /// Acquire a HOST_VISIBLE uniform buffer (UBO).
    ///
    /// Returns a [`crate::core::rhi::UniformBuffer`] — the type system
    /// enforces that this buffer can only be bound to a kernel's
    /// `set_uniform_buffer` slot (not storage / vertex / index).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        tracing::debug!(
            rhi_op = "acquire_uniform_buffer",
            byte_size,
            "GpuContext::acquire_uniform_buffer"
        );
        let vulkan_device = &self.device.inner;
        crate::core::rhi::UniformBuffer::new_host_visible(vulkan_device, byte_size)
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    ///
    /// Returns a [`crate::core::rhi::VertexBuffer`] — only bindable to
    /// `set_vertex_buffer` slots.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        tracing::debug!(
            rhi_op = "acquire_vertex_buffer",
            byte_size,
            "GpuContext::acquire_vertex_buffer"
        );
        let vulkan_device = &self.device.inner;
        crate::core::rhi::VertexBuffer::new_host_visible(vulkan_device, byte_size)
    }

    /// Acquire a HOST_VISIBLE index buffer.
    ///
    /// Returns a [`crate::core::rhi::IndexBuffer`] — only bindable to
    /// `set_index_buffer` slots.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        tracing::debug!(
            rhi_op = "acquire_index_buffer",
            byte_size,
            "GpuContext::acquire_index_buffer"
        );
        let vulkan_device = &self.device.inner;
        crate::core::rhi::IndexBuffer::new_host_visible(vulkan_device, byte_size)
    }

    /// Import an IOSurface's memory as a storage buffer, zero-copy, retaining
    /// the surface for the buffer's life. Refused, naming the reason, when
    /// the device cannot import host memory, the surface's base address is
    /// off the import alignment, or the driver declines (MoltenVK before
    /// 1.4.1 does). Backs
    /// [`GpuContextFullAccess::import_iosurface_as_storage_buffer`].
    #[cfg(target_os = "macos")]
    #[tracing::instrument(
        level = "debug",
        skip(self, iosurface),
        fields(rhi_op = "import_iosurface_as_storage_buffer")
    )]
    pub fn import_iosurface_as_storage_buffer(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> Result<crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer> {
        crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer::import(&self.device.inner, iosurface)
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
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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

    /// Pre-allocate a ring of `count` non-exportable DEVICE_LOCAL
    /// textures and register each in the same-process texture cache.
    /// Mirror of [`GpuContextFullAccess::create_texture_ring`] at the
    /// inner-`GpuContext` level — the FullAccess wrapper delegates here
    /// after enforcing the privileged-scope invariants.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_texture_ring(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
        usages: TextureUsages,
        count: usize,
    ) -> Result<crate::core::context::TextureRing> {
        use crate::core::context::{TextureRing, TextureRingInner, TextureRingSlot};

        if count == 0 {
            return Err(Error::GpuError(
                "create_texture_ring: count must be > 0".into(),
            ));
        }

        let mut slots = Vec::with_capacity(count);
        let mut upload_resources = Vec::with_capacity(count);
        for slot_index in 0..count {
            let desc = TextureDescriptor::new(width, height, format).with_usage(usages);
            let texture = self.device.create_texture_local(&desc)?;
            let surface_id = uuid::Uuid::new_v4().to_string();
            // Spec-correct initial layout for a freshly-allocated
            // VkImage that no one has touched yet (per
            // texture registration's producer
            // Rule 2). The per-frame
            // `TextureRing::copy_pixel_buffer_to_slot` runs the
            // amortized upload that transitions UNDEFINED →
            // SHADER_READ_ONLY_OPTIMAL and updates the registration
            // to match, so after the first per-frame copy the claim
            // and reality agree.
            self.register_texture_with_layout(
                &surface_id,
                texture.clone(),
                VulkanLayout::UNDEFINED,
            );
            slots.push(TextureRingSlot::new(
                texture,
                &surface_id,
                slot_index as u32,
            ));
            let res = crate::vulkan::rhi::HostVulkanUploadResources::new(&self.device.inner)?;
            upload_resources.push(res);
        }
        let inner_arc = TextureRingInner::from_slots(
            slots,
            upload_resources,
            width,
            height,
            format,
            self.clone(),
        );
        Ok(TextureRing::from_arc_into_raw(inner_arc))
    }

    /// Import a DMA-BUF FD as a `StorageBuffer`. Camera V4L2 zero-copy
    /// path. Consumes `dma_buf_fd` whatever the outcome: the driver owns
    /// it behind a successful import and a failure closes it.
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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

    /// Transition `texture` into `VK_IMAGE_LAYOUT_GENERAL` via a
    /// one-shot command buffer + fence. Used as the prelude to binding
    /// a freshly-created storage image to a compute / RT kernel that
    /// will write into it via `imageStore`. The transition uses
    /// `UNDEFINED` as the source layout, so this is correct for
    /// just-allocated textures only — once the texture has content
    /// you'd otherwise lose, callers must use a barrier with the
    /// actual prior layout.
    ///
    /// Lives here (not on `HostVulkanTexture`) so example / processor
    /// code that needs a one-shot layout transition stays inside the
    /// RHI boundary instead of pulling vulkanalia directly. Mirrors
    /// the existing `acquire_*` shape on `GpuContext`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn transition_storage_image_to_general(
        &self,
        texture: &crate::core::rhi::Texture,
    ) -> Result<()> {
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::HostVulkanTexture::transition_to_general(
            vulkan_device,
            texture.vulkan_inner().image().ok_or_else(|| {
                crate::core::Error::GpuError(
                    "transition_storage_image_to_general: texture missing VkImage".to_string(),
                )
            })?,
        )
    }

    /// Create a host-side texture-readback handle bound to a fixed
    /// format/extent. The staging buffer + command resources + timeline
    /// semaphore are allocated once at construction and reused across
    /// every submit. Single-in-flight per handle (mirroring
    /// [`crate::vulkan::rhi::VulkanComputeKernel`]); for parallel
    /// readbacks, hold N handles.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_texture_readback(
        &self,
        descriptor: &crate::core::rhi::TextureReadbackDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::VulkanTextureReadback>> {
        tracing::debug!(
            rhi_op = "create_texture_readback",
            label = descriptor.label,
            format = ?descriptor.format,
            width = descriptor.width,
            height = descriptor.height,
            bytes = descriptor.staging_size(),
            "GpuContext::create_texture_readback"
        );
        let vulkan_device = &self.device.inner;
        let handle = crate::vulkan::rhi::VulkanTextureReadback::new_into_stream_error(
            vulkan_device,
            descriptor,
        )?;
        Ok(Arc::new(handle))
    }
}
