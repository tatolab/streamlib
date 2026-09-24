// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Typed `VkImageUsageFlags` newtype for image usage crossing the wire.

use vulkanalia::vk;

/// `VkImageUsageFlags` bits as a registration's `vk_image_usage` carries
/// them, so a caller without a `vulkanalia` import can hand an image's usage
/// across and an importer can refuse bits it does not know.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct VulkanImageUsage(pub u32);

impl VulkanImageUsage {
    /// The flags, or `None` when a bit is not a `VkImageUsageFlagBits` this
    /// build knows — an image built with it dropped would differ from the
    /// registrant's.
    pub fn as_vk(self) -> Option<vk::ImageUsageFlags> {
        vk::ImageUsageFlags::from_bits(self.0)
    }

    /// Wrap `flags`.
    pub fn from_vk(flags: vk::ImageUsageFlags) -> Self {
        Self(flags.bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_bits_round_trip_and_an_unknown_bit_is_refused() {
        let usage = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::STORAGE;
        assert_eq!(VulkanImageUsage::from_vk(usage).as_vk(), Some(usage));
        assert_eq!(VulkanImageUsage(1 << 31).as_vk(), None);
    }
}
