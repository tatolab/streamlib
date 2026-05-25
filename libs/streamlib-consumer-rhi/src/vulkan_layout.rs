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
///
/// `#[repr(transparent)]` so the newtype is byte-equivalent to its
/// inner `i32` across the plugin FFI boundary — adapter vtables (see
/// `streamlib-adapter-{vulkan,cuda,cpu-readback}`) carry
/// `initial_layout_raw: i32` / `post_release_layout_raw: i32` fields
/// that get reconstituted as `VulkanLayout(raw)` on the receiving
/// side. Pinning the repr means a cdylib compiled with a different
/// rustc / dep-graph than the host still observes the same byte
/// layout for any cross-DSO traffic.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
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

#[cfg(test)]
mod layout_tests {
    //! Layout regression tests for the FFI-crossing layout primitive.
    //!
    //! `#[repr(transparent)]` over the `i32` `VkImageLayout` value, so
    //! the newtype is byte-equivalent to its inner field — adapter
    //! vtables in `streamlib-adapter-{vulkan,cuda,cpu-readback}` carry
    //! `initial_layout_raw: i32` / `post_release_layout_raw: i32`
    //! arguments that get reconstituted as `VulkanLayout(raw)` on the
    //! receiving side. The known-constant values below pin the
    //! Vulkan-spec discriminants the carve-out relies on; a silent
    //! drift in vulkanalia's `vk::ImageLayout::*.as_raw()` mapping
    //! would fail loudly here.
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn vulkan_layout_layout() {
        // Size + align alone would also hold under Rust's default
        // single-field tuple layout, so these asserts mostly lock
        // against a future "second field added" drift; the
        // missing-`#[repr(transparent)]` regression itself is caught
        // at the workspace level by `cargo xtask
        // check-consumer-rhi-repr`. The constants + round-trip tests
        // below are the load-bearing wire-contract locks.
        assert_eq!(size_of::<VulkanLayout>(), size_of::<i32>());
        assert_eq!(align_of::<VulkanLayout>(), align_of::<i32>());
    }

    #[test]
    fn vulkan_layout_constants_match_vk_spec() {
        // Vulkan spec — `VK_IMAGE_LAYOUT_*` enumerants. Locked so a
        // vulkanalia upgrade can't silently re-number them.
        assert_eq!(VulkanLayout::UNDEFINED.0, 0);
        assert_eq!(VulkanLayout::GENERAL.0, 1);
        assert_eq!(VulkanLayout::COLOR_ATTACHMENT_OPTIMAL.0, 2);
        assert_eq!(VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0, 5);
        assert_eq!(VulkanLayout::TRANSFER_SRC_OPTIMAL.0, 6);
        assert_eq!(VulkanLayout::TRANSFER_DST_OPTIMAL.0, 7);
    }

    #[test]
    fn vulkan_layout_round_trips_through_vk() {
        for layout in [
            VulkanLayout::UNDEFINED,
            VulkanLayout::GENERAL,
            VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
        ] {
            assert_eq!(VulkanLayout(layout.as_vk().as_raw()), layout);
        }
    }
}
