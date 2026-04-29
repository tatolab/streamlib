// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Typed `VkImageLayout` newtype shared by every surface adapter.
//!
//! Lives in consumer-rhi so adapters and cdylibs that import via
//! consumer-rhi get a single source of truth — no parallel `VulkanLayout`
//! definition per adapter crate.

use vulkanalia::vk;

/// `VkImageLayout` enumerant. Stored as `i32` per the Vulkan spec.
///
/// The newtype wraps the raw value so adapter APIs don't drag every
/// caller through a `vulkanalia` import — cdylibs and polyglot
/// scenario binaries can construct registrations using
/// [`VulkanLayout::UNDEFINED`] / [`VulkanLayout::GENERAL`] / etc.
/// without depending on `vulkanalia`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VulkanLayout(pub i32);

impl VulkanLayout {
    pub const UNDEFINED: Self = Self(vk::ImageLayout::UNDEFINED.as_raw());
    pub const GENERAL: Self = Self(vk::ImageLayout::GENERAL.as_raw());
    pub const COLOR_ATTACHMENT_OPTIMAL: Self =
        Self(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw());
    pub const SHADER_READ_ONLY_OPTIMAL: Self =
        Self(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL.as_raw());
    pub const TRANSFER_SRC_OPTIMAL: Self =
        Self(vk::ImageLayout::TRANSFER_SRC_OPTIMAL.as_raw());
    pub const TRANSFER_DST_OPTIMAL: Self =
        Self(vk::ImageLayout::TRANSFER_DST_OPTIMAL.as_raw());

    /// Convert to the underlying `vk::ImageLayout`. Adapter crates use
    /// this when issuing layout transitions; cdylibs never call it
    /// because they don't depend on `vulkanalia`.
    pub fn as_vk(self) -> vk::ImageLayout {
        vk::ImageLayout::from_raw(self.0)
    }
}
