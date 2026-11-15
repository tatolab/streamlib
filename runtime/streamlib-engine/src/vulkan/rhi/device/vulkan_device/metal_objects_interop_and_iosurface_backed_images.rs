// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::sync::atomic::Ordering;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::TextureDescriptor;
use crate::core::{Error, Result};
use crate::vulkan::rhi::HostVulkanTexture;

use super::HostVulkanDevice;

impl HostVulkanDevice {
    /// Whether `VK_EXT_metal_objects` was enabled — what an image over an
    /// IOSurface and a timeline's Metal shared event both need.
    pub fn supports_metal_objects_interop(&self) -> bool {
        self.device
            .extensions()
            .contains(&vk::EXT_METAL_OBJECTS_EXTENSION.name)
    }

    /// Create a texture over a fresh private IOSurface — see
    /// [`HostVulkanTexture::new_iosurface_backed`].
    pub fn create_texture_iosurface_backed(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new_iosurface_backed(self, desc)
    }

    /// Memory for an image created over an IOSurface, raw `vkAllocateMemory`
    /// like the imports above, on the type
    /// [`streamlib_consumer_rhi::device_local_memory_type_that_is_not_host_visible`]
    /// picks.
    pub fn allocate_device_local_memory_for_an_iosurface_backed_image(
        &self,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
    ) -> Result<vk::DeviceMemory> {
        let memory_type_index =
            streamlib_consumer_rhi::device_local_memory_type_that_is_not_host_visible(
                &self.memory_properties,
                memory_type_bits,
            )
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "allocate_device_local_memory_for_an_iosurface_backed_image: no device-local \
                 memory type without host visibility is in the image's memory_type_bits \
                 ({memory_type_bits:#x})"
                ))
            })?;
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .build();
        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "allocate_device_local_memory_for_an_iosurface_backed_image: the driver refused \
                 {allocation_size} bytes on memory type {memory_type_index}: {e}"
            ))
        })?;
        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            allocation_size,
            memory_type_index,
            live = count,
            "HostVulkanDevice: memory bound for an IOSurface-backed image"
        );
        Ok(memory)
    }
}
