// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The device a codec session is minted on: one a codec module owns.

use std::sync::{Arc, MutexGuard};

use vulkanalia::vk;
use vulkanalia_vma as vma;

use streamlib_consumer_rhi::{OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection};

use crate::{VideoError, VideoResult};

/// One queue a codec submits to, and the family that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VulkanVideoCodecQueue {
    pub queue: vk::Queue,
    pub queue_family_index: u32,
}

/// What the codec layer needs of a Vulkan device: its handles, its allocator,
/// the queues it encodes, decodes, converts and copies on, and the
/// synchronization its submits and allocations ride under.
pub trait VulkanVideoCodecDevice: Send + Sync {
    /// The instance the device was created on.
    fn instance(&self) -> &vulkanalia::Instance;
    /// The logical device.
    fn device(&self) -> &vulkanalia::Device;
    /// The physical device the logical device was created on.
    fn physical_device(&self) -> vk::PhysicalDevice;
    /// The allocator every codec resource comes from.
    fn allocator(&self) -> &Arc<vma::Allocator>;
    /// The Vulkan Video encode queue, where the device enabled encode.
    fn video_encode_queue(&self) -> Option<VulkanVideoCodecQueue>;
    /// The Vulkan Video decode queue, where the device enabled decode.
    fn video_decode_queue(&self) -> Option<VulkanVideoCodecQueue>;
    /// The queue the RGB → NV12 conversion dispatches on.
    fn compute_queue(&self) -> VulkanVideoCodecQueue;
    /// The queue staging uploads and layout transitions run on.
    fn transfer_queue(&self) -> VulkanVideoCodecQueue;
    /// Submit under the device's per-queue synchronization.
    ///
    /// # Safety
    /// The standard `vkQueueSubmit2` preconditions, with `queue` one of this
    /// device's own.
    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> VideoResult<()>;
    /// Hold the device-level resource lock an allocation or session creation
    /// runs under.
    fn lock_device(&self) -> MutexGuard<'_, ()>;
    /// `vkDeviceWaitIdle` under the synchronization the spec requires.
    fn wait_idle(&self) -> VideoResult<()>;
}

impl VulkanVideoCodecDevice for OwnedVulkanVideoCodecDevice {
    fn instance(&self) -> &vulkanalia::Instance {
        self.consumer_vulkan_device().instance()
    }

    fn device(&self) -> &vulkanalia::Device {
        self.consumer_vulkan_device().device()
    }

    fn physical_device(&self) -> vk::PhysicalDevice {
        self.consumer_vulkan_device().physical_device()
    }

    fn allocator(&self) -> &Arc<vma::Allocator> {
        OwnedVulkanVideoCodecDevice::allocator(self)
    }

    fn video_encode_queue(&self) -> Option<VulkanVideoCodecQueue> {
        the_video_codec_queue_of(self, VulkanVideoCodecDirection::Encode)
    }

    fn video_decode_queue(&self) -> Option<VulkanVideoCodecQueue> {
        the_video_codec_queue_of(self, VulkanVideoCodecDirection::Decode)
    }

    fn compute_queue(&self) -> VulkanVideoCodecQueue {
        the_import_queue_of(self)
    }

    fn transfer_queue(&self) -> VulkanVideoCodecQueue {
        the_import_queue_of(self)
    }

    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> VideoResult<()> {
        unsafe {
            self.consumer_vulkan_device()
                .submit_to_queue(queue, submits, fence)
        }
        .map_err(VideoError::from)
    }

    fn lock_device(&self) -> MutexGuard<'_, ()> {
        OwnedVulkanVideoCodecDevice::lock_device(self)
    }

    fn wait_idle(&self) -> VideoResult<()> {
        self.consumer_vulkan_device()
            .wait_idle()
            .map_err(VideoError::from)
    }
}

/// The owned device's video queue, when it codes in `direction`.
fn the_video_codec_queue_of(
    owned_device: &OwnedVulkanVideoCodecDevice,
    direction: VulkanVideoCodecDirection,
) -> Option<VulkanVideoCodecQueue> {
    let video_codec_queue = owned_device.video_codec_queue();
    (video_codec_queue.direction == direction).then_some(VulkanVideoCodecQueue {
        queue: video_codec_queue.queue,
        queue_family_index: video_codec_queue.queue_family_index,
    })
}

/// The owned device's one GRAPHICS queue, which also computes and copies.
fn the_import_queue_of(owned_device: &OwnedVulkanVideoCodecDevice) -> VulkanVideoCodecQueue {
    VulkanVideoCodecQueue {
        queue: owned_device.consumer_vulkan_device().queue(),
        queue_family_index: owned_device.consumer_vulkan_device().queue_family_index(),
    }
}

/// Whether the device enabled Vulkan Video encode.
pub(crate) fn supports_video_encode(codec_device: &dyn VulkanVideoCodecDevice) -> bool {
    codec_device.video_encode_queue().is_some()
}

/// Whether the device enabled Vulkan Video decode.
pub(crate) fn supports_video_decode(codec_device: &dyn VulkanVideoCodecDevice) -> bool {
    codec_device.video_decode_queue().is_some()
}
