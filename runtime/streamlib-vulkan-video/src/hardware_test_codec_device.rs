// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The device the hardware-gated tests mint codec resources on.

use std::sync::Arc;

use streamlib_consumer_rhi::{
    OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection,
    vulkan_device_uuid_of_the_first_device_the_loader_enumerates,
};

use crate::VulkanVideoCodecDevice;

/// An owned video-encode device on the first GPU the loader enumerates, or
/// `None` when this machine has no Vulkan Video encode.
pub(crate) fn owned_video_encode_device_on_the_first_gpu() -> Option<Arc<dyn VulkanVideoCodecDevice>>
{
    owned_video_codec_device_on_the_first_gpu(VulkanVideoCodecDirection::Encode)
}

/// An owned video-decode device on the first GPU the loader enumerates, or
/// `None` when this machine has no Vulkan Video decode.
pub(crate) fn owned_video_decode_device_on_the_first_gpu() -> Option<Arc<dyn VulkanVideoCodecDevice>>
{
    owned_video_codec_device_on_the_first_gpu(VulkanVideoCodecDirection::Decode)
}

fn owned_video_codec_device_on_the_first_gpu(
    direction: VulkanVideoCodecDirection,
) -> Option<Arc<dyn VulkanVideoCodecDevice>> {
    let device_uuid = match vulkan_device_uuid_of_the_first_device_the_loader_enumerates() {
        Ok(device_uuid) => device_uuid,
        Err(error) => {
            tracing::warn!("Skipping — no Vulkan device available: {error}");
            return None;
        }
    };
    match OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(device_uuid, direction) {
        Ok(owned_device) => Some(Arc::new(owned_device)),
        Err(error) => {
            tracing::warn!("Skipping — no Vulkan Video {direction:?} device available: {error}");
            None
        }
    }
}
