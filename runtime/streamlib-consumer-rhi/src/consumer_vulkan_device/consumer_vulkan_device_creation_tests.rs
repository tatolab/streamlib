// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::vk;

use super::consumer_vulkan_device_creation::logical_device_creation_refusal;
use crate::ConsumerRhiError;

#[test]
fn a_device_the_driver_lacked_a_resource_for_is_refused_for_now() {
    for refusal in [
        vk::ErrorCode::INITIALIZATION_FAILED,
        vk::ErrorCode::OUT_OF_DEVICE_MEMORY,
        vk::ErrorCode::OUT_OF_HOST_MEMORY,
        vk::ErrorCode::TOO_MANY_OBJECTS,
    ] {
        let error = logical_device_creation_refusal(refusal);
        assert!(
            matches!(error, ConsumerRhiError::GpuOutOfResourcesForNow(_)),
            "{refusal:?} answered {error:?}"
        );
        assert!(
            error
                .to_string()
                .starts_with("GPU operation failed: Failed to create consumer logical device: "),
            "{error}"
        );
    }
}

#[test]
fn a_device_the_gpu_cannot_give_is_refused_for_good() {
    for refusal in [
        vk::ErrorCode::EXTENSION_NOT_PRESENT,
        vk::ErrorCode::FEATURE_NOT_PRESENT,
        vk::ErrorCode::DEVICE_LOST,
    ] {
        let error = logical_device_creation_refusal(refusal);
        assert!(
            matches!(error, ConsumerRhiError::Gpu(_)),
            "{refusal:?} answered {error:?}"
        );
    }
}
