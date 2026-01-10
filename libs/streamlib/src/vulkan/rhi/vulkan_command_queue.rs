// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan command queue wrapper for RHI.

use crate::core::{Result, StreamError};

use super::VulkanCommandBuffer;

/// Vulkan command queue wrapper.
///
/// This is a stub implementation. Full Vulkan support is pending.
pub struct VulkanCommandQueue {
    pub(crate) _private: (),
}

impl VulkanCommandQueue {
    /// Create a new command buffer from this queue.
    pub fn create_command_buffer(&self) -> Result<VulkanCommandBuffer> {
        Err(StreamError::GpuError(
            "Vulkan command buffer creation not implemented".into(),
        ))
    }
}
