// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan command buffer implementation for RHI.

use super::VulkanTexture;

/// Vulkan command buffer wrapper.
///
/// This is a stub implementation. Full Vulkan support is pending.
pub struct VulkanCommandBuffer {
    _private: (),
}

impl VulkanCommandBuffer {
    /// Copy one texture to another.
    pub fn copy_texture(&mut self, _src: &VulkanTexture, _dst: &VulkanTexture) {
        // Stub - no-op
    }

    /// Commit the command buffer for execution.
    pub fn commit(self) {
        // Stub - no-op
    }

    /// Commit and wait for completion.
    pub fn commit_and_wait(self) {
        // Stub - no-op
    }
}
