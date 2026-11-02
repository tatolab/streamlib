// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine device context the Vulkan Video decoder mints its resources
//! against.

use std::sync::Arc;

use streamlib_vulkan_video::{
    VideoResult, VulkanVideoCodecDevice, create_nv12_ycbcr_conversion_for_video_image_views,
};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

/// The engine's device as the decoder reaches it: the RHI handle its
/// converters build on, the same device as the codec layer's
/// [`VulkanVideoCodecDevice`] its sessions, DPBs and bitstream buffers are
/// minted on, and the NV12 conversion its image views chain.
pub struct VideoContext {
    host_device: Arc<crate::vulkan::rhi::HostVulkanDevice>,
    codec_device: Arc<dyn VulkanVideoCodecDevice>,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    nv12_ycbcr_conversion: vk::SamplerYcbcrConversion,
}

impl VideoContext {
    /// Wrap the engine's device for the decoder.
    pub fn new(host_device: Arc<crate::vulkan::rhi::HostVulkanDevice>) -> VideoResult<Self> {
        let memory_properties = unsafe {
            host_device
                .instance()
                .get_physical_device_memory_properties(host_device.physical_device())
        };
        let nv12_ycbcr_conversion =
            create_nv12_ycbcr_conversion_for_video_image_views(host_device.device())?;
        let codec_device: Arc<dyn VulkanVideoCodecDevice> = host_device.clone();
        Ok(Self {
            host_device,
            codec_device,
            memory_properties,
            nv12_ycbcr_conversion,
        })
    }

    /// The logical device.
    pub fn device(&self) -> &vulkanalia::Device {
        self.host_device.device()
    }

    /// The instance the device was created on.
    pub fn instance(&self) -> &vulkanalia::Instance {
        self.host_device.instance()
    }

    /// The physical device.
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.host_device.physical_device()
    }

    /// The physical device's memory properties.
    pub fn memory_properties(&self) -> &vk::PhysicalDeviceMemoryProperties {
        &self.memory_properties
    }

    /// The allocator every decode resource comes from.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        self.host_device.allocator()
    }

    /// The engine RHI handle the decode-side converters build on.
    pub fn host_device(&self) -> &Arc<crate::vulkan::rhi::HostVulkanDevice> {
        &self.host_device
    }

    /// The same device, as the codec layer mints sessions, DPBs and
    /// bitstream buffers on it.
    pub fn codec_device(&self) -> &Arc<dyn VulkanVideoCodecDevice> {
        &self.codec_device
    }

    /// The NV12 conversion chained onto multi-planar image views whose image
    /// includes `SAMPLED` usage (`VUID-VkImageViewCreateInfo-format-06415`).
    pub fn nv12_ycbcr_conversion(&self) -> vk::SamplerYcbcrConversion {
        self.nv12_ycbcr_conversion
    }
}

impl Drop for VideoContext {
    fn drop(&mut self) {
        unsafe {
            self.device()
                .destroy_sampler_ycbcr_conversion(self.nv12_ycbcr_conversion, None);
        }
    }
}
