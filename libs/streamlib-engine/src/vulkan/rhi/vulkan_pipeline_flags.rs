// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Typed wrappers over `VkPipelineStageFlags2` and `VkAccessFlags2`.
//!
//! Engine-internal — used by [`RhiCommandRecorder`](super::RhiCommandRecorder)
//! so consumers can issue barriers without importing `vulkanalia`. Mirrors
//! [`VulkanLayout`](crate::core::rhi::VulkanLayout)'s shape (newtype over the
//! raw bits, named `pub const` entries for the variants in use). New variants
//! get added as consumers need them; bitwise `|` is supported so callers can
//! combine flags.
//!
//! [`RhiCommandRecorder`]: super::RhiCommandRecorder

use vulkanalia::vk;

/// Typed `VkPipelineStageFlags2`. Stored as the raw `u64` bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VulkanStage(pub u64);

impl VulkanStage {
    pub const NONE: Self = Self(vk::PipelineStageFlags2::NONE.bits());
    pub const TOP_OF_PIPE: Self = Self(vk::PipelineStageFlags2::TOP_OF_PIPE.bits());
    pub const BOTTOM_OF_PIPE: Self = Self(vk::PipelineStageFlags2::BOTTOM_OF_PIPE.bits());
    pub const ALL_COMMANDS: Self = Self(vk::PipelineStageFlags2::ALL_COMMANDS.bits());
    pub const ALL_GRAPHICS: Self = Self(vk::PipelineStageFlags2::ALL_GRAPHICS.bits());
    pub const ALL_TRANSFER: Self = Self(vk::PipelineStageFlags2::ALL_TRANSFER.bits());
    pub const COPY: Self = Self(vk::PipelineStageFlags2::COPY.bits());
    pub const BLIT: Self = Self(vk::PipelineStageFlags2::BLIT.bits());
    pub const COMPUTE_SHADER: Self = Self(vk::PipelineStageFlags2::COMPUTE_SHADER.bits());
    pub const VERTEX_SHADER: Self = Self(vk::PipelineStageFlags2::VERTEX_SHADER.bits());
    pub const FRAGMENT_SHADER: Self = Self(vk::PipelineStageFlags2::FRAGMENT_SHADER.bits());
    pub const COLOR_ATTACHMENT_OUTPUT: Self =
        Self(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT.bits());
    pub const HOST: Self = Self(vk::PipelineStageFlags2::HOST.bits());

    pub fn as_vk(self) -> vk::PipelineStageFlags2 {
        vk::PipelineStageFlags2::from_bits_truncate(self.0)
    }

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
    pub const NONE: Self = Self(vk::AccessFlags2::NONE.bits());
    pub const SHADER_READ: Self = Self(vk::AccessFlags2::SHADER_READ.bits());
    pub const SHADER_WRITE: Self = Self(vk::AccessFlags2::SHADER_WRITE.bits());
    pub const TRANSFER_READ: Self = Self(vk::AccessFlags2::TRANSFER_READ.bits());
    pub const TRANSFER_WRITE: Self = Self(vk::AccessFlags2::TRANSFER_WRITE.bits());
    pub const HOST_READ: Self = Self(vk::AccessFlags2::HOST_READ.bits());
    pub const HOST_WRITE: Self = Self(vk::AccessFlags2::HOST_WRITE.bits());
    pub const COLOR_ATTACHMENT_READ: Self =
        Self(vk::AccessFlags2::COLOR_ATTACHMENT_READ.bits());
    pub const COLOR_ATTACHMENT_WRITE: Self =
        Self(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE.bits());
    pub const MEMORY_READ: Self = Self(vk::AccessFlags2::MEMORY_READ.bits());
    pub const MEMORY_WRITE: Self = Self(vk::AccessFlags2::MEMORY_WRITE.bits());

    pub fn as_vk(self) -> vk::AccessFlags2 {
        vk::AccessFlags2::from_bits_truncate(self.0)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_const_round_trips_through_vulkanalia() {
        assert_eq!(
            VulkanStage::COMPUTE_SHADER.as_vk(),
            vk::PipelineStageFlags2::COMPUTE_SHADER
        );
        assert_eq!(
            VulkanStage::ALL_TRANSFER.as_vk(),
            vk::PipelineStageFlags2::ALL_TRANSFER
        );
        assert_eq!(VulkanStage::HOST.as_vk(), vk::PipelineStageFlags2::HOST);
        assert_eq!(VulkanStage::NONE.as_vk(), vk::PipelineStageFlags2::NONE);
    }

    #[test]
    fn access_const_round_trips_through_vulkanalia() {
        assert_eq!(
            VulkanAccess::SHADER_WRITE.as_vk(),
            vk::AccessFlags2::SHADER_WRITE
        );
        assert_eq!(
            VulkanAccess::TRANSFER_READ.as_vk(),
            vk::AccessFlags2::TRANSFER_READ
        );
        assert_eq!(VulkanAccess::HOST_READ.as_vk(), vk::AccessFlags2::HOST_READ);
        assert_eq!(VulkanAccess::NONE.as_vk(), vk::AccessFlags2::NONE);
    }

    #[test]
    fn stage_bitor_combines() {
        let combined = VulkanStage::COMPUTE_SHADER | VulkanStage::ALL_TRANSFER;
        assert_eq!(
            combined.as_vk(),
            vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::ALL_TRANSFER
        );
    }

    #[test]
    fn access_bitor_combines() {
        let combined = VulkanAccess::SHADER_WRITE | VulkanAccess::TRANSFER_READ;
        assert_eq!(
            combined.as_vk(),
            vk::AccessFlags2::SHADER_WRITE | vk::AccessFlags2::TRANSFER_READ
        );
    }
}
