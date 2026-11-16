// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The device's handles, its identity, and the import capabilities its enabled extensions give it.

use super::*;

impl ConsumerVulkanDevice {
    /// The loader's `vkGetInstanceProcAddr`, valid while this device lives.
    #[cfg(target_os = "linux")]
    pub(crate) fn get_instance_proc_addr(&self) -> vk::PFN_vkGetInstanceProcAddr {
        self.get_instance_proc_addr
    }

    /// The `VkPhysicalDeviceFeatures` this device was created with.
    #[cfg(target_os = "linux")]
    pub(crate) fn enabled_physical_device_features(&self) -> &vk::PhysicalDeviceFeatures {
        &self.enabled_physical_device_features
    }

    /// `VkPhysicalDeviceIDProperties::deviceUUID` for the selected
    /// physical device. CUDA / OpenCL interop matches against
    /// `cudaDeviceProp::uuid` to pick the foreign-API device that
    /// shares this Vulkan device's PCI identity.
    pub fn physical_device_uuid(&self) -> [u8; 16] {
        self.physical_device_uuid
    }

    /// The driver's `minImportedHostPointerAlignment`, or `None` when this
    /// device cannot import host memory.
    pub fn imported_host_pointer_alignment(&self) -> Option<vk::DeviceSize> {
        self.imported_host_pointer_alignment
    }

    /// Device label from `VkPhysicalDeviceProperties::deviceName`.
    pub fn name(&self) -> &str {
        &self.device_name
    }

    /// Whether the physical device is a CPU-type software rasterizer.
    pub fn device_is_software(&self) -> bool {
        self.device_is_software
    }

    /// Whether `VK_EXT_external_memory_dma_buf` was enabled — what every
    /// DMA-BUF memory import needs; never on a software device.
    pub fn supports_dma_buf_memory_import(&self) -> bool {
        device_extension_is_enabled(&self.device, vk::EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION)
    }

    /// Whether `VK_EXT_image_drm_format_modifier` was enabled — what an
    /// image over a DMA-BUF with an explicit DRM modifier needs.
    pub fn supports_drm_format_modifier_image_import(&self) -> bool {
        device_extension_is_enabled(&self.device, vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_EXTENSION)
    }

    /// Whether `VK_KHR_external_semaphore_fd` was enabled — what importing a
    /// timeline semaphore's OPAQUE_FD needs.
    pub fn supports_opaque_fd_timeline_semaphore_import(&self) -> bool {
        device_extension_is_enabled(&self.device, vk::KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION)
    }

    /// Refuse `refusing_operation` by name, naming this device, unless
    /// `device_extension` was enabled on it.
    pub(crate) fn refuse_unless_device_extension_is_enabled(
        &self,
        refusing_operation: &str,
        device_extension: vk::Extension,
        what_cannot_happen_without_it: &str,
    ) -> Result<()> {
        if device_extension_is_enabled(&self.device, device_extension) {
            return Ok(());
        }
        Err(ConsumerRhiError::Gpu(format!(
            "{refusing_operation}: {} is not enabled on this device ({}), so \
             {what_cannot_happen_without_it}",
            device_extension.name, self.device_name
        )))
    }

    /// The property flags of memory type `memory_type_index`, or `None` when
    /// the device has no such type.
    pub fn property_flags_of_memory_type_index(
        &self,
        memory_type_index: u32,
    ) -> Option<vk::MemoryPropertyFlags> {
        (memory_type_index < self.memory_properties.memory_type_count)
            .then(|| self.memory_properties.memory_types[memory_type_index as usize].property_flags)
    }

    /// Loaded Vulkan entry points.
    pub fn entry(&self) -> &vulkanalia::Entry {
        &self.entry
    }

    /// Vulkan instance owned by this consumer device.
    pub fn instance(&self) -> &vulkanalia::Instance {
        &self.instance
    }

    /// Selected physical device.
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    /// Logical Vulkan device — exposed for the carve-out's import +
    /// bind paths (creating an image / buffer, querying memory
    /// requirements, building command buffers for layout transitions).
    pub fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    /// Default submit queue for layout transitions + sync work.
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }

    /// Queue family that owns [`Self::queue`].
    pub fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    /// The Vulkan Video encode or decode queue, on a device created with one.
    pub fn video_codec_queue(&self) -> Option<ConsumerVulkanVideoCodecQueue> {
        self.video_codec_queue
    }

    /// `vkDeviceWaitIdle` under the queue mutex, which the spec requires to
    /// be held over every queue the device owns.
    pub fn wait_idle(&self) -> Result<()> {
        let _lock = self.queue_mutex.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.device_wait_idle() }
            .map_err(|e| ConsumerRhiError::Gpu(format!("device_wait_idle failed: {e}")))
    }
}
