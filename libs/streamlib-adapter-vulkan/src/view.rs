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
    VkImageHandle, VkImageInfo, VkImageLayoutValue, VulkanImageInfoExt, VulkanWritable,
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

// GPU surface views must not implement `CpuReadable`. Switching to the
// `streamlib-adapter-cpu-readback` adapter is the contractual signal for
// "I want CPU bytes" — see #514. Adding the impl back, even returning an
// empty slice "for symmetry," would re-introduce the asymmetry that PR
// #527 deliberately removed and would make every Vulkan consumer
// silently appear CPU-readable.
mod _assert_vulkan_read_view_not_cpu_readable {
    use super::VulkanReadView;
    use streamlib_adapter_abi::CpuReadable;

    trait AmbiguousIfImpl<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
    #[allow(dead_code)]
    struct Invalid;
    impl<T: ?Sized + CpuReadable> AmbiguousIfImpl<Invalid> for T {}

    const _: fn() = || {
        let _ = <VulkanReadView<'static> as AmbiguousIfImpl<_>>::some_item;
    };
}
