// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan device implementation for RHI.

use crate::core::rhi::TextureDescriptor;
use crate::core::{Result, StreamError};

use super::{VulkanCommandQueue, VulkanTexture};

/// Vulkan GPU device.
///
/// This is a stub implementation. Full Vulkan support is pending.
pub struct VulkanDevice {
    _private: (),
}

impl VulkanDevice {
    /// Create a new Vulkan device.
    ///
    /// Note: This is a stub that returns an error until Vulkan is fully implemented.
    pub fn new() -> Result<Self> {
        Err(StreamError::GpuError(
            "Vulkan backend not yet implemented. Use Metal on macOS/iOS.".into(),
        ))
    }

    /// Create a texture on this device.
    pub fn create_texture(&self, _desc: &TextureDescriptor) -> Result<VulkanTexture> {
        Err(StreamError::GpuError(
            "Vulkan texture creation not implemented".into(),
        ))
    }

    /// Create a VulkanCommandQueue wrapper for the shared command queue.
    pub fn create_command_queue_wrapper(&self) -> VulkanCommandQueue {
        VulkanCommandQueue { _private: () }
    }

    /// Get the device name.
    #[allow(dead_code)]
    pub fn name(&self) -> String {
        "Vulkan (not implemented)".into()
    }
}
