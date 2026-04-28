// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Capability markers and the small device-shape trait surface adapters
//! abstract over.
//!
//! [`HostMarker`] and [`ConsumerMarker`] are the privilege flavors a
//! Vulkan-typed RHI resource can be parameterized on. They feed into the
//! source-level split (`HostVulkanDevice` / `ConsumerVulkanDevice` and
//! the parametric texture / pixel-buffer / timeline-semaphore types) so
//! the FullAccess capability boundary is enforced by the type system,
//! not by convention.
//!
//! [`VulkanRhiDevice`] is the minimal set of methods every surface
//! adapter needs at its layout-transition seam: a logical device, the
//! queue it submits to, that queue's family index, and a mutex-protected
//! submit. Both flavors implement it; adapter code is generic over
//! `D: VulkanRhiDevice<Privilege = …>` and works against either.

use vulkanalia::vk;

use crate::core::Result;

mod sealed {
    pub trait Sealed {}
}

/// Privilege marker for host-side Vulkan resources — full RHI access
/// (allocation, queue submit, modifier choice, kernel construction,
/// swapchain).
pub struct HostMarker;
impl sealed::Sealed for HostMarker {}

/// Privilege marker for consumer-side Vulkan resources — carve-out
/// only. See `docs/architecture/subprocess-rhi-parity.md`: DMA-BUF FD
/// import + bind + map, tiled-image import via
/// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`, layout transitions on
/// imported handles, sync wait/signal on imported timeline semaphores.
pub struct ConsumerMarker;
impl sealed::Sealed for ConsumerMarker {}

/// Sealed trait restricting privilege markers to [`HostMarker`] and
/// [`ConsumerMarker`].
pub trait DevicePrivilege: sealed::Sealed + 'static + Send + Sync {}
impl DevicePrivilege for HostMarker {}
impl DevicePrivilege for ConsumerMarker {}

/// Minimal device shape every surface adapter needs at the layout-
/// transition + submit seam.
///
/// Implementations expose their raw `vulkanalia::Device` so adapters
/// can record command buffers, but capability enforcement happens at
/// the **crate boundary**: `streamlib-consumer-rhi` only re-exports
/// the consumer-flavored device, so a cdylib depending on it cannot
/// reach the host's `VkDevice` (the one tied to swapchains, host VMA
/// pools, encoder/decoder queues, and modifier-probing state).
pub trait VulkanRhiDevice: Send + Sync {
    /// Privilege marker — `HostMarker` or `ConsumerMarker`.
    type Privilege: DevicePrivilege;

    /// Logical Vulkan device.
    fn device(&self) -> &vulkanalia::Device;

    /// Default submit queue.
    fn queue(&self) -> vk::Queue;

    /// Queue family that owns [`Self::queue`].
    fn queue_family_index(&self) -> u32;

    /// Submit command buffers under the device's per-queue mutex.
    /// Vulkan requires external synchronization for `vkQueueSubmit2`
    /// against the same `VkQueue` from multiple threads; the host's
    /// per-queue mutex is the canonical way streamlib serializes that.
    /// Consumer-side devices can use a single mutex since they only
    /// hold one queue.
    ///
    /// # Safety
    /// Caller must satisfy the standard `vkQueueSubmit2` preconditions
    /// (valid `submits`, valid optional `fence`, no concurrent native
    /// submits to `queue` outside this trait).
    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()>;
}
