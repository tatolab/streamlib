// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire scope.
//!
//! The views are short-lived (lifetime-bound to the guard) and implement
//! the capability traits from `streamlib-adapter-abi` so outer adapters
//! (`streamlib-adapter-skia`, third-party Vulkan-on-Vulkan compositions)
//! can compose on top without touching DMA-BUF fds or layout state
//! directly.

use std::marker::PhantomData;

use streamlib_adapter_abi::{
    CpuReadable, VkImageHandle, VkImageInfo, VkImageLayoutValue, VulkanImageInfoExt,
    VulkanWritable,
};
use vulkanalia::vk;
use vulkanalia::vk::Handle as _;

/// Read view of an acquired surface — exposes the host's `VkImage`,
/// the layout the adapter transitioned it to (`SHADER_READ_ONLY_OPTIMAL`),
/// and full image-info for outer adapters that need to wrap it.
pub struct VulkanReadView<'g> {
    pub(crate) image: vk::Image,
    pub(crate) layout: vk::ImageLayout,
    pub(crate) info: VkImageInfo,
    /// Optional CPU-side staging slice. Some adapter consumers (the
    /// in-process round-trip tests, debug snapshotters) want a byte
    /// view of the surface in addition to the `VkImage`. The adapter
    /// fills this when the consumer asks for it; the default is
    /// `None`.
    pub(crate) cpu_bytes: Option<&'g [u8]>,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl VulkanWritable for VulkanReadView<'_> {
    fn vk_image(&self) -> VkImageHandle {
        VkImageHandle(self.image.as_raw())
    }

    fn vk_image_layout(&self) -> VkImageLayoutValue {
        VkImageLayoutValue(self.layout.as_raw())
    }
}

impl VulkanImageInfoExt for VulkanReadView<'_> {
    fn vk_image_info(&self) -> VkImageInfo {
        self.info
    }
}

impl CpuReadable for VulkanReadView<'_> {
    fn read_bytes(&self) -> &[u8] {
        self.cpu_bytes.unwrap_or(&[])
    }
}

/// Write view of an acquired surface — exposes the host's `VkImage`
/// transitioned to `COLOR_ATTACHMENT_OPTIMAL` (or `GENERAL` when the
/// surface usage demands it), plus full image-info for compositors.
pub struct VulkanWriteView<'g> {
    pub(crate) image: vk::Image,
    pub(crate) layout: vk::ImageLayout,
    pub(crate) info: VkImageInfo,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl VulkanWritable for VulkanWriteView<'_> {
    fn vk_image(&self) -> VkImageHandle {
        VkImageHandle(self.image.as_raw())
    }

    fn vk_image_layout(&self) -> VkImageLayoutValue {
        VkImageLayoutValue(self.layout.as_raw())
    }
}

impl VulkanImageInfoExt for VulkanWriteView<'_> {
    fn vk_image_info(&self) -> VkImageInfo {
        self.info
    }
}
