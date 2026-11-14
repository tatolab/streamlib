// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The device context an encode session's resources are minted against.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::{VideoResult, VulkanVideoCodecDevice};

/// The NV12 conversion handle chained onto video-coding image views.
///
/// Its chroma offsets are inert: this handle is only ever attached to image
/// views (DPB, encode-src, staging), where a multi-planar view requires a
/// conversion but no sample operation ever reads the offsets.
pub fn create_nv12_ycbcr_conversion_for_video_image_views(
    device: &vulkanalia::Device,
) -> VideoResult<vk::SamplerYcbcrConversion> {
    let info = vk::SamplerYcbcrConversionCreateInfo::builder()
        .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
        .ycbcr_model(vk::SamplerYcbcrModelConversion::YCBCR_709)
        .ycbcr_range(vk::SamplerYcbcrRange::ITU_NARROW)
        .components(vk::ComponentMapping {
            r: vk::ComponentSwizzle::IDENTITY,
            g: vk::ComponentSwizzle::IDENTITY,
            b: vk::ComponentSwizzle::IDENTITY,
            a: vk::ComponentSwizzle::IDENTITY,
        })
        .x_chroma_offset(vk::ChromaLocation::MIDPOINT)
        .y_chroma_offset(vk::ChromaLocation::MIDPOINT)
        .chroma_filter(vk::Filter::LINEAR)
        .force_explicit_reconstruction(false);
    Ok(unsafe { device.create_sampler_ycbcr_conversion(&info, None)? })
}

/// A codec device plus the NV12 conversion every encode image view chains.
pub struct VulkanVideoEncodeContext {
    codec_device: Arc<dyn VulkanVideoCodecDevice>,
    nv12_ycbcr_conversion: vk::SamplerYcbcrConversion,
}

impl VulkanVideoEncodeContext {
    /// Create the context's NV12 conversion on `codec_device`.
    pub fn new(codec_device: Arc<dyn VulkanVideoCodecDevice>) -> VideoResult<Self> {
        let nv12_ycbcr_conversion =
            create_nv12_ycbcr_conversion_for_video_image_views(codec_device.device())?;
        Ok(Self {
            codec_device,
            nv12_ycbcr_conversion,
        })
    }

    /// The device every encode resource is minted on.
    pub fn codec_device(&self) -> &Arc<dyn VulkanVideoCodecDevice> {
        &self.codec_device
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

    /// The allocator every encode resource comes from.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        self.codec_device.allocator()
    }

    /// The NV12 conversion chained onto multi-planar video image views whose
    /// image includes `SAMPLED` usage (`VUID-VkImageViewCreateInfo-format-06415`).
    pub fn nv12_ycbcr_conversion(&self) -> vk::SamplerYcbcrConversion {
        self.nv12_ycbcr_conversion
    }
}

impl Drop for VulkanVideoEncodeContext {
    fn drop(&mut self) {
        unsafe {
            self.device()
                .destroy_sampler_ycbcr_conversion(self.nv12_ycbcr_conversion, None);
        }
    }
}
