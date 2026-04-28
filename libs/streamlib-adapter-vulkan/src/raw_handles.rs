// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib.vulkan.raw_handles()` — escape hatch for power-user
//! customers that need direct Vulkan handle access (custom RHIs,
//! integration with another engine, debug tooling).
//!
//! The `acquire_*` API on [`crate::VulkanContext`] is the path of least
//! resistance and handles sync + layout transitions; this is the "you
//! own the footguns from here" surface.

use streamlib::adapter_support::HostVulkanDevice;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

/// Raw Vulkan handles surfaced by `raw_handles()`.
///
/// Every field is `u64` (Vulkan handle width per spec) so this struct
/// can cross any binding boundary — `ash::Image::from_raw`,
/// `vulkanalia::vk::Image::from_raw`, custom FFI shims, polyglot SDKs.
/// The customer reconstructs the typed wrapper their binding wants.
#[derive(Clone, Copy, Debug)]
pub struct RawVulkanHandles {
    /// `VkInstance` handle.
    pub vk_instance: u64,
    /// `VkPhysicalDevice` handle.
    pub vk_physical_device: u64,
    /// `VkDevice` handle.
    pub vk_device: u64,
    /// `VkQueue` of the graphics-and-present queue family. Caller MUST
    /// take the per-queue mutex if they intend to submit work — the
    /// streamlib RHI does this internally; raw users assume the
    /// responsibility.
    pub vk_queue: u64,
    /// Queue family index for `vk_queue`. Used in
    /// `VkImageMemoryBarrier::srcQueueFamilyIndex` /
    /// `dstQueueFamilyIndex` for cross-queue ownership transitions.
    pub vk_queue_family_index: u32,
    /// Vulkan API version the streamlib runtime requested at instance
    /// creation. Encoded per `VK_MAKE_API_VERSION`.
    pub api_version: u32,
}

/// Snapshot the underlying `HostVulkanDevice`'s raw handles.
///
/// The handles are valid for the lifetime of the device; the customer
/// MUST NOT outlive the runtime that owns it. There is intentionally
/// no destructor or refcount handed back — this is the documented
/// power-user surface that says *"you own the consequences"*.
pub fn raw_handles(device: &HostVulkanDevice) -> RawVulkanHandles {
    RawVulkanHandles {
        vk_instance: device.instance().handle().as_raw() as u64,
        vk_physical_device: device.physical_device().as_raw() as u64,
        vk_device: device.device().handle().as_raw() as u64,
        vk_queue: device.queue().as_raw() as u64,
        vk_queue_family_index: device.queue_family_index(),
        api_version: vk::make_version(1, 4, 0),
    }
}
