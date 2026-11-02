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
/// inner `i32` across the plugin ABI — adapter vtables (see
/// `streamlib-adapter-{vulkan,cuda,cpu-readback}`) carry
/// `initial_layout_raw: i32` / `post_release_layout_raw: i32` fields
/// that get reconstituted as `VulkanLayout(raw)` on the receiving
/// side. Pinning the repr means a cdylib compiled with a different
/// rustc / dep-graph than the host still observes the same byte
/// layout for any plugin ABI traffic.
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
    pub const TRANSFER_SRC_OPTIMAL: Self = Self(vk::ImageLayout::TRANSFER_SRC_OPTIMAL.as_raw());
    pub const TRANSFER_DST_OPTIMAL: Self = Self(vk::ImageLayout::TRANSFER_DST_OPTIMAL.as_raw());

    /// Convert to the underlying `vk::ImageLayout`. Adapter crates use
    /// this when issuing layout transitions; cdylibs never call it
    /// because they don't depend on `vulkanalia`.
    pub fn as_vk(self) -> vk::ImageLayout {
        vk::ImageLayout::from_raw(self.0)
    }
}

/// A layout a sampled image may be read in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampledImageReadLayout {
    /// `SHADER_READ_ONLY_OPTIMAL`, which resolved published surfaces are left in.
    ShaderReadOnlyOptimal,
    /// `GENERAL`, which a compute kernel's output is left in.
    General,
}

impl SampledImageReadLayout {
    /// The Vulkan layout written into the sampled-image descriptor.
    pub fn as_vk(self) -> vk::ImageLayout {
        match self {
            Self::ShaderReadOnlyOptimal => vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            Self::General => vk::ImageLayout::GENERAL,
        }
    }

    /// The layout a reader samples an image published in `published_layout`
    /// in: its own when a sampled image may be read in it, otherwise
    /// `SHADER_READ_ONLY_OPTIMAL`, which the reader barriers it into first.
    /// `UNDEFINED` is refused — a transition out of it may discard the picture.
    pub fn to_read_an_image_published_in(published_layout: VulkanLayout) -> crate::Result<Self> {
        if published_layout == VulkanLayout::UNDEFINED {
            return Err(crate::ConsumerRhiError::Gpu(
                "an image published in UNDEFINED (0) has no picture a barrier keeps — no \
                 producer published a layout for it"
                    .to_string(),
            ));
        }
        Ok(Self::sampled_read_layout_matching(published_layout)
            .unwrap_or(Self::ShaderReadOnlyOptimal))
    }

    fn sampled_read_layout_matching(layout: VulkanLayout) -> Option<Self> {
        match layout {
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL => Some(Self::ShaderReadOnlyOptimal),
            VulkanLayout::GENERAL => Some(Self::General),
            _ => None,
        }
    }
}

impl TryFrom<VulkanLayout> for SampledImageReadLayout {
    type Error = crate::ConsumerRhiError;

    fn try_from(layout: VulkanLayout) -> crate::Result<Self> {
        Self::sampled_read_layout_matching(layout).ok_or_else(|| {
            crate::ConsumerRhiError::Gpu(format!(
                "a sampled image is read in SHADER_READ_ONLY_OPTIMAL ({}) or GENERAL ({}), not \
                 layout {}",
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0,
                VulkanLayout::GENERAL.0,
                layout.0
            ))
        })
    }
}

impl From<SampledImageReadLayout> for VulkanLayout {
    fn from(layout: SampledImageReadLayout) -> Self {
        match layout {
            SampledImageReadLayout::ShaderReadOnlyOptimal => Self::SHADER_READ_ONLY_OPTIMAL,
            SampledImageReadLayout::General => Self::GENERAL,
        }
    }
}

#[cfg(test)]
mod layout_tests {
    //! Layout regression tests for the plugin-ABI-crossing layout primitive.
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

    #[test]
    fn a_sampled_image_is_read_in_shader_read_only_optimal_or_general_and_nothing_else() {
        assert_eq!(
            SampledImageReadLayout::try_from(VulkanLayout::SHADER_READ_ONLY_OPTIMAL).unwrap(),
            SampledImageReadLayout::ShaderReadOnlyOptimal
        );
        assert_eq!(
            SampledImageReadLayout::try_from(VulkanLayout::GENERAL).unwrap(),
            SampledImageReadLayout::General
        );
        let refusal = SampledImageReadLayout::try_from(VulkanLayout::TRANSFER_DST_OPTIMAL)
            .expect_err("a sampled image is never read in TRANSFER_DST_OPTIMAL");
        assert!(
            refusal
                .to_string()
                .contains("SHADER_READ_ONLY_OPTIMAL (5) or GENERAL (1)"),
            "{refusal}"
        );
    }

    #[test]
    fn a_published_image_is_read_in_its_own_layout_or_barriered_into_shader_read_only_optimal() {
        for (published, read_in) in [
            (
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                SampledImageReadLayout::ShaderReadOnlyOptimal,
            ),
            (VulkanLayout::GENERAL, SampledImageReadLayout::General),
            (
                VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
                SampledImageReadLayout::ShaderReadOnlyOptimal,
            ),
            (
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                SampledImageReadLayout::ShaderReadOnlyOptimal,
            ),
            (
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                SampledImageReadLayout::ShaderReadOnlyOptimal,
            ),
        ] {
            assert_eq!(
                SampledImageReadLayout::to_read_an_image_published_in(published).unwrap(),
                read_in,
                "an image published in layout {}",
                published.0
            );
        }
        for read_in in [
            SampledImageReadLayout::ShaderReadOnlyOptimal,
            SampledImageReadLayout::General,
        ] {
            assert_eq!(
                SampledImageReadLayout::try_from(VulkanLayout::from(read_in)).unwrap(),
                read_in
            );
        }
        let refusal =
            SampledImageReadLayout::to_read_an_image_published_in(VulkanLayout::UNDEFINED)
                .expect_err("a transition out of UNDEFINED may discard the picture");
        assert!(refusal.to_string().contains("UNDEFINED"), "{refusal}");
    }
}
