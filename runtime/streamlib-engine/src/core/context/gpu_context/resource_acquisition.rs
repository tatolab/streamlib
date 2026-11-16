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
