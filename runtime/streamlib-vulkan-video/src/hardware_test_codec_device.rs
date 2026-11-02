// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The device the hardware-gated tests mint codec resources on.

use std::sync::Arc;

use streamlib_consumer_rhi::{
    OwnedVulkanVideoEncodeDevice, vulkan_device_uuid_of_the_first_device_the_loader_enumerates,
};

use crate::VulkanVideoCodecDevice;

/// An owned video-encode device on the first GPU the loader enumerates, or
/// `None` when this machine has no Vulkan Video encode.
pub(crate) fn owned_video_encode_device_on_the_first_gpu() -> Option<Arc<dyn VulkanVideoCodecDevice>>
{
    let device_uuid = match vulkan_device_uuid_of_the_first_device_the_loader_enumerates() {
        Ok(device_uuid) => device_uuid,
        Err(error) => {
            tracing::warn!("Skipping — no Vulkan device available: {error}");
            return None;
        }
    };
    match OwnedVulkanVideoEncodeDevice::new_on_the_device_the_runtime_named(device_uuid) {
        Ok(owned_device) => Some(Arc::new(owned_device)),
        Err(error) => {
            tracing::warn!("Skipping — no Vulkan Video encode device available: {error}");
            None
        }
    }
}
