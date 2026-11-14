// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The device context the Vulkan Video decoder mints its resources against.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::{
    VideoResult, VulkanVideoCodecDevice, create_nv12_ycbcr_conversion_for_video_image_views,
};

/// The codec device as the decoder reaches it: the device its sessions, DPBs,
/// bitstream buffers and NV12 → RGBA converter are minted on, its memory
/// properties, and the NV12 conversion its image views chain.
pub struct VulkanVideoDecodeContext {
    codec_device: Arc<dyn VulkanVideoCodecDevice>,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    nv12_ycbcr_conversion: vk::SamplerYcbcrConversion,
}

impl VulkanVideoDecodeContext {
    /// Wrap `codec_device` for the decoder.
    pub fn new(codec_device: Arc<dyn VulkanVideoCodecDevice>) -> VideoResult<Self> {
        let memory_properties = unsafe {
            codec_device
                .instance()
                .get_physical_device_memory_properties(codec_device.physical_device())
        };
        let nv12_ycbcr_conversion =
            create_nv12_ycbcr_conversion_for_video_image_views(codec_device.device())?;
        Ok(Self {
            codec_device,
            memory_properties,
            nv12_ycbcr_conversion,
        })
    }

    /// The logical device.
    pub fn device(&self) -> &vulkanalia::Device {
        self.codec_device.device()
    }

    /// The instance the device was created on.
    pub fn instance(&self) -> &vulkanalia::Instance {
        self.codec_device.instance()
    }

    /// The physical device.
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.codec_device.physical_device()
    }

    /// The physical device's memory properties.
    pub fn memory_properties(&self) -> &vk::PhysicalDeviceMemoryProperties {
        &self.memory_properties
    }

    /// The allocator every decode resource comes from.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        self.codec_device.allocator()
    }

    /// The device sessions, DPBs and bitstream buffers are minted on, and
    /// every submit goes through.
    pub fn codec_device(&self) -> &Arc<dyn VulkanVideoCodecDevice> {
        &self.codec_device
    }

    /// The NV12 conversion chained onto multi-planar image views whose image
    /// includes `SAMPLED` usage (`VUID-VkImageViewCreateInfo-format-06415`).
    pub fn nv12_ycbcr_conversion(&self) -> vk::SamplerYcbcrConversion {
        self.nv12_ycbcr_conversion
    }
}

impl Drop for VulkanVideoDecodeContext {
    fn drop(&mut self) {
        unsafe {
            self.device()
                .destroy_sampler_ycbcr_conversion(self.nv12_ycbcr_conversion, None);
        }
    }
}
