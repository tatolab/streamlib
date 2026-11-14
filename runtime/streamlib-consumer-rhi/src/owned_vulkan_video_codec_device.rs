// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The owned-device tier: a Vulkan device a module creates for itself on the
//! runtime's GPU, with a video encode or decode queue and an allocator of its
//! own.
//!
//! The device is a [`ConsumerVulkanDevice`] created with a Vulkan Video
//! queue, so every import the carve-out offers lands on it unchanged: frames
//! move between the runtime and this device by check-out and import, and the
//! codec allocates its own sessions, reference pictures and bitstream buffers
//! through [`Self::allocator`]. A driver fault in the video engine then ends
//! the module's process, never the runtime's device.

use std::sync::{Arc, Mutex, MutexGuard};

use vulkanalia_vma as vma;

use crate::{
    ConsumerRhiError, ConsumerVulkanDevice, ConsumerVulkanVideoCodecQueue, Result,
    VulkanVideoCodecDirection,
};

/// A module-owned Vulkan device with a video encode or decode queue — see
/// the module docs.
pub struct OwnedVulkanVideoCodecDevice {
    /// Declared first so it drops before the device it allocates from.
    allocator: Arc<vma::Allocator>,
    video_codec_queue: ConsumerVulkanVideoCodecQueue,
    /// Serializes allocation and session creation against submits, the
    /// discipline NVIDIA Linux needs between concurrent device-level calls.
    device_resource_lock: Mutex<()>,
    consumer_vulkan_device: Arc<ConsumerVulkanDevice>,
}

impl OwnedVulkanVideoCodecDevice {
    /// Create the device on the GPU whose `deviceUUID` is
    /// `vulkan_device_uuid_the_runtime_named`, refusing by name when it
    /// cannot code in `direction` with Vulkan Video.
    pub fn new_on_the_device_the_runtime_named(
        vulkan_device_uuid_the_runtime_named: [u8; 16],
        direction: VulkanVideoCodecDirection,
    ) -> Result<Self> {
        let consumer_vulkan_device = Arc::new(
            ConsumerVulkanDevice::new_with_a_video_codec_queue_on_the_device_the_runtime_named(
                vulkan_device_uuid_the_runtime_named,
                direction,
            )?,
        );
        let video_codec_queue = consumer_vulkan_device.video_codec_queue().ok_or_else(|| {
            ConsumerRhiError::Gpu(format!(
                "{} was created without the video {direction:?} queue it was asked for",
                consumer_vulkan_device.name()
            ))
        })?;
        let mut allocator_options = vma::AllocatorOptions::new(
            consumer_vulkan_device.instance(),
            consumer_vulkan_device.device(),
            consumer_vulkan_device.physical_device(),
        );
        allocator_options.version = vulkanalia::Version::new(1, 4, 0);
        let allocator = Arc::new(unsafe { vma::Allocator::new(&allocator_options) }.map_err(
            |allocator_failure| {
                ConsumerRhiError::Gpu(format!(
                    "the owned device's allocator could not be created: {allocator_failure}"
                ))
            },
        )?);
        tracing::info!(
            device = consumer_vulkan_device.name(),
            ?direction,
            video_codec_queue_family_index = video_codec_queue.queue_family_index,
            supports_h265 = video_codec_queue.supports_h265,
            "owned Vulkan Video device created on the device the runtime named"
        );
        Ok(Self {
            allocator,
            video_codec_queue,
            device_resource_lock: Mutex::new(()),
            consumer_vulkan_device,
        })
    }

    /// The device frames are imported onto and the codec records on.
    pub fn consumer_vulkan_device(&self) -> &Arc<ConsumerVulkanDevice> {
        &self.consumer_vulkan_device
    }

    /// The allocator the codec's own resources come from.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        &self.allocator
    }

    /// The video queue, the way it codes, and its family.
    pub fn video_codec_queue(&self) -> ConsumerVulkanVideoCodecQueue {
        self.video_codec_queue
    }

    /// Hold the device-level resource lock for an allocation or session
    /// creation.
    pub fn lock_device(&self) -> MutexGuard<'_, ()> {
        self.device_resource_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
