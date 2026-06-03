// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-free twins of `VkPipelineStageFlags2` / `VkAccessFlags2`.
//!
//! Mirror the engine's `vulkan/rhi/vulkan_pipeline_flags.rs` newtypes so
//! a cdylib can issue barriers through
//! [`crate::rhi::RhiCommandRecorder::record_image_barrier`] without
//! importing `vulkanalia` (banned in the `plugin/` zone). The const
//! values are the stable Vulkan-spec `VkPipelineStageFlags2` /
//! `VkAccessFlags2` bit positions; the recorder vtable carries the raw
//! `u64` bits across the plugin ABI and the host reconstructs the typed
//! `vk::*` flags internally.

/// Typed `VkPipelineStageFlags2`. Stored as the raw `u64` bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VulkanStage(pub u64);

impl VulkanStage {
    pub const NONE: Self = Self(0);
    pub const TOP_OF_PIPE: Self = Self(1);
    pub const BOTTOM_OF_PIPE: Self = Self(1 << 13);
    pub const ALL_COMMANDS: Self = Self(1 << 16);
    pub const ALL_GRAPHICS: Self = Self(1 << 15);
    pub const ALL_TRANSFER: Self = Self(1 << 12);
    pub const COPY: Self = Self(1 << 32);
    pub const BLIT: Self = Self(1 << 34);
    pub const COMPUTE_SHADER: Self = Self(1 << 11);
    pub const VERTEX_SHADER: Self = Self(1 << 3);
    pub const FRAGMENT_SHADER: Self = Self(1 << 7);
    pub const COLOR_ATTACHMENT_OUTPUT: Self = Self(1 << 10);
    pub const HOST: Self = Self(1 << 14);

    /// Raw `VkPipelineStageFlags2` bits.
    pub fn bits(self) -> u64 {
        self.0
    }
}

impl std::ops::BitOr for VulkanStage {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for VulkanStage {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Typed `VkAccessFlags2`. Stored as the raw `u64` bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VulkanAccess(pub u64);

impl VulkanAccess {
    pub const NONE: Self = Self(0);
    pub const SHADER_READ: Self = Self(1 << 5);
    pub const SHADER_SAMPLED_READ: Self = Self(1 << 32);
    pub const SHADER_WRITE: Self = Self(1 << 6);
    pub const TRANSFER_READ: Self = Self(1 << 11);
    pub const TRANSFER_WRITE: Self = Self(1 << 12);
    pub const HOST_READ: Self = Self(1 << 13);
    pub const HOST_WRITE: Self = Self(1 << 14);
    pub const COLOR_ATTACHMENT_READ: Self = Self(1 << 7);
    pub const COLOR_ATTACHMENT_WRITE: Self = Self(1 << 8);
    pub const MEMORY_READ: Self = Self(1 << 15);
    pub const MEMORY_WRITE: Self = Self(1 << 16);

    /// Raw `VkAccessFlags2` bits.
    pub fn bits(self) -> u64 {
        self.0
    }
}

impl std::ops::BitOr for VulkanAccess {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for VulkanAccess {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

// =============================================================================
// ABI value-lock tests
// =============================================================================
//
// These constants are hand-transcribed `VkPipelineStageFlags2` / `VkAccessFlags2`
// bit positions and cross the plugin ABI as raw `u64` to the host, which feeds
// them straight into `vk::*Flags2`. A wrong bit is a silently-wrong barrier
// (GPU hazard / corruption), not a compile error — so lock each value to the
// Vulkan spec here. The engine's `vulkan/rhi/vulkan_pipeline_flags.rs` locks the
// same constants to `vulkanalia` (= the same spec values); both pinned to the
// spec means host and plugin agree. Drift on either side fails a test.
#[cfg(test)]
mod abi_value_lock {
    use super::*;

    #[test]
    fn pipeline_stage_bits_match_vulkan_spec() {
        assert_eq!(VulkanStage::NONE.bits(), 0);
        assert_eq!(VulkanStage::TOP_OF_PIPE.bits(), 0x1);
        assert_eq!(VulkanStage::VERTEX_SHADER.bits(), 0x8);
        assert_eq!(VulkanStage::FRAGMENT_SHADER.bits(), 0x80);
        assert_eq!(VulkanStage::COLOR_ATTACHMENT_OUTPUT.bits(), 0x400);
        assert_eq!(VulkanStage::COMPUTE_SHADER.bits(), 0x800);
        assert_eq!(VulkanStage::ALL_TRANSFER.bits(), 0x1000);
        assert_eq!(VulkanStage::BOTTOM_OF_PIPE.bits(), 0x2000);
        assert_eq!(VulkanStage::HOST.bits(), 0x4000);
        assert_eq!(VulkanStage::ALL_GRAPHICS.bits(), 0x8000);
        assert_eq!(VulkanStage::ALL_COMMANDS.bits(), 0x10000);
        assert_eq!(VulkanStage::COPY.bits(), 0x1_0000_0000);
        assert_eq!(VulkanStage::BLIT.bits(), 0x4_0000_0000);
    }

    #[test]
    fn access_bits_match_vulkan_spec() {
        assert_eq!(VulkanAccess::NONE.bits(), 0);
        assert_eq!(VulkanAccess::SHADER_READ.bits(), 0x20);
        assert_eq!(VulkanAccess::SHADER_WRITE.bits(), 0x40);
        assert_eq!(VulkanAccess::COLOR_ATTACHMENT_READ.bits(), 0x80);
        assert_eq!(VulkanAccess::COLOR_ATTACHMENT_WRITE.bits(), 0x100);
        assert_eq!(VulkanAccess::TRANSFER_READ.bits(), 0x800);
        assert_eq!(VulkanAccess::TRANSFER_WRITE.bits(), 0x1000);
        assert_eq!(VulkanAccess::HOST_READ.bits(), 0x2000);
        assert_eq!(VulkanAccess::HOST_WRITE.bits(), 0x4000);
        assert_eq!(VulkanAccess::MEMORY_READ.bits(), 0x8000);
        assert_eq!(VulkanAccess::MEMORY_WRITE.bits(), 0x10000);
        assert_eq!(VulkanAccess::SHADER_SAMPLED_READ.bits(), 0x1_0000_0000);
    }
}
