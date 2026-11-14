// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A codec's Decoded Picture Buffer image, bound to its video profile.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc as _};

use crate::{VideoError, VideoResult, VulkanVideoCodecDevice};

/// Whether a DPB serves a decoder or an encoder — the usage bit the driver
/// requires on its image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDpbDirection {
    Decode,
    Encode,
}

/// Inputs for [`VulkanVideoDpbImage::new`].
///
/// `array_layers` is the slot count for one layered image, or `1` for one of
/// the separate per-slot images a driver advertising
/// `SEPARATE_REFERENCE_IMAGES` takes. Two or more `sharing_queue_families`
/// make the image `CONCURRENT` across them. `additional_usage` is everything
/// beyond the direction's DPB bit. The extent is pre-aligned to the profile's
/// `pictureAccessGranularity` by the caller.
pub struct VideoDpbImageDescriptor<'a> {
    pub label: &'a str,
    pub width: u32,
    pub height: u32,
    pub format: vk::Format,
    pub array_layers: u32,
    pub direction: VideoDpbDirection,
    pub additional_usage: vk::ImageUsageFlags,
    pub sharing_queue_families: &'a [u32],
    pub video_profile: &'a vk::VideoProfileInfoKHR,
}

/// A device-local DPB `VkImage` and its allocation, freed on drop.
pub struct VulkanVideoDpbImage {
    codec_device: Arc<dyn VulkanVideoCodecDevice>,
    image: vk::Image,
    allocation: vma::Allocation,
}

// SAFETY: the image is only destroyed on drop; every read goes through `&self`.
unsafe impl Send for VulkanVideoDpbImage {}
unsafe impl Sync for VulkanVideoDpbImage {}

impl VulkanVideoDpbImage {
    /// Allocate the image under the device's resource lock.
    pub fn new(
        codec_device: &Arc<dyn VulkanVideoCodecDevice>,
        descriptor: &VideoDpbImageDescriptor<'_>,
    ) -> VideoResult<Self> {
        if descriptor.width == 0 || descriptor.height == 0 || descriptor.array_layers == 0 {
            return Err(VideoError::Configuration(format!(
                "VulkanVideoDpbImage::new ({}): width, height, array_layers must be > 0; got \
                 {}x{}x{}",
                descriptor.label, descriptor.width, descriptor.height, descriptor.array_layers,
            )));
        }
        let dpb_flag = match descriptor.direction {
            VideoDpbDirection::Decode => vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR,
            VideoDpbDirection::Encode => vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
        };
        let mut profile_list = vk::VideoProfileListInfoKHR::builder()
            .profiles(std::slice::from_ref(descriptor.video_profile));
        let mut image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(descriptor.format)
            .extent(vk::Extent3D {
                width: descriptor.width,
                height: descriptor.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(descriptor.array_layers)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(dpb_flag | descriptor.additional_usage)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut profile_list);
        image_info = if descriptor.sharing_queue_families.len() > 1 {
            image_info
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(descriptor.sharing_queue_families)
        } else {
            image_info.sharing_mode(vk::SharingMode::EXCLUSIVE)
        };
        let alloc_options = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let _device_lock = codec_device.lock_device();
        let (image, allocation) = unsafe {
            codec_device
                .allocator()
                .create_image(image_info, &alloc_options)
        }
        .map_err(|e| {
            VideoError::Rhi(format!(
                "VulkanVideoDpbImage::new ({}): vmaCreateImage failed: {e}",
                descriptor.label,
            ))
        })?;
        Ok(Self {
            codec_device: Arc::clone(codec_device),
            image,
            allocation,
        })
    }

    /// The `VkImage`, valid until this value drops.
    pub fn image(&self) -> vk::Image {
        self.image
    }
}

impl Drop for VulkanVideoDpbImage {
    fn drop(&mut self) {
        unsafe {
            self.codec_device
                .allocator()
                .destroy_image(self.image, self.allocation)
        };
    }
}
