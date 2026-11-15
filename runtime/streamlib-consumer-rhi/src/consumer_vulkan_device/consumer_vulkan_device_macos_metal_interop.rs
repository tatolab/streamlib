// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Metal interop on macOS: whether it is enabled, and device-local memory for an image over an IOSurface.

use super::*;

impl ConsumerVulkanDevice {
    /// Whether `VK_EXT_metal_objects` was enabled — what an image over an
    /// IOSurface and a timeline over a Metal shared event both need.
    pub fn supports_metal_objects_interop(&self) -> bool {
        device_extension_is_enabled(&self.device, vk::EXT_METAL_OBJECTS_EXTENSION)
    }

    /// Memory for an image created over an IOSurface, on the type
    /// [`crate::device_local_memory_type_that_is_not_host_visible`] picks.
    /// Pairs with [`Self::free_imported_memory`].
    pub fn allocate_device_local_memory_for_an_iosurface_backed_image(
        &self,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
    ) -> Result<vk::DeviceMemory> {
        let memory_type_index = crate::device_local_memory_type_that_is_not_host_visible(
            &self.memory_properties,
            memory_type_bits,
        )
        .ok_or_else(|| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: no device-local memory type without host visibility \
                     is in the image's memory_type_bits ({memory_type_bits:#x})"
            ))
        })?;
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .build();
        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: the driver refused {allocation_size} bytes on memory type \
                 {memory_type_index} for an IOSurface-backed image: {e}"
            ))
        })?;
        self.live_allocation_count.fetch_add(1, Ordering::Relaxed);
        Ok(memory)
    }
}
