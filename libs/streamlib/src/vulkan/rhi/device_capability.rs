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
///
/// Carries the timeline-semaphore + texture associated types so adapter
/// code that holds `Arc<P::TimelineSemaphore>` / `Arc<P::Texture>`
/// resolves to the right concrete types at instantiation. Both flavors
/// implement the corresponding `*Like` trait so the adapter can call
/// `wait` / `signal_host` / `image` without knowing which side it's on.
pub trait DevicePrivilege: sealed::Sealed + 'static + Send + Sync {
    /// Concrete timeline-semaphore type for this privilege flavor.
    type TimelineSemaphore: VulkanTimelineSemaphoreLike + Send + Sync + 'static;
    /// Concrete texture type for this privilege flavor.
    type Texture: VulkanTextureLike + Send + Sync + 'static;
}

#[cfg(target_os = "linux")]
impl DevicePrivilege for HostMarker {
    type TimelineSemaphore = super::HostVulkanTimelineSemaphore;
    type Texture = super::HostVulkanTexture;
}

#[cfg(target_os = "linux")]
impl DevicePrivilege for ConsumerMarker {
    type TimelineSemaphore = super::ConsumerVulkanTimelineSemaphore;
    type Texture = super::ConsumerVulkanTexture;
}

// Non-Linux: HostMarker still resolves but to phantom unit types for
// platforms where the DMA-BUF / OPAQUE_FD machinery isn't built.
// ConsumerMarker only exists on Linux today.
#[cfg(not(target_os = "linux"))]
impl DevicePrivilege for HostMarker {
    type TimelineSemaphore = NotAvailableOnThisPlatform;
    type Texture = NotAvailableOnThisPlatform;
}

/// Phantom type for platforms where DMA-BUF / OPAQUE_FD primitives
/// aren't built. Stays uninstantiable so trait bounds resolve at
/// type-check time but no caller can construct one.
#[cfg(not(target_os = "linux"))]
pub enum NotAvailableOnThisPlatform {}

#[cfg(not(target_os = "linux"))]
impl VulkanTimelineSemaphoreLike for NotAvailableOnThisPlatform {
    fn wait(&self, _value: u64, _timeout_ns: u64) -> crate::core::Result<()> {
        match *self {}
    }
    fn signal_host(&self, _value: u64) -> crate::core::Result<()> {
        match *self {}
    }
}

#[cfg(not(target_os = "linux"))]
impl VulkanTextureLike for NotAvailableOnThisPlatform {
    fn image(&self) -> Option<vk::Image> {
        match *self {}
    }
    fn chosen_drm_format_modifier(&self) -> u64 {
        match *self {}
    }
}

/// Operations the surface adapter needs from a timeline semaphore. Both
/// [`crate::vulkan::rhi::HostVulkanTimelineSemaphore`] and
/// [`crate::vulkan::rhi::ConsumerVulkanTimelineSemaphore`] implement
/// this with delegating bodies — the trait exists so `VulkanSurfaceAdapter`
/// can be generic over the device flavor without dynamic dispatch.
pub trait VulkanTimelineSemaphoreLike {
    /// Block until the timeline counter reaches or exceeds `value`.
    fn wait(&self, value: u64, timeout_ns: u64) -> crate::core::Result<()>;
    /// Host-side signal: advance the counter to `value`.
    fn signal_host(&self, value: u64) -> crate::core::Result<()>;
}

/// Operations the surface adapter needs from a Vulkan-flavored texture.
/// Both [`crate::vulkan::rhi::HostVulkanTexture`] and
/// [`crate::vulkan::rhi::ConsumerVulkanTexture`] implement this; the
/// adapter holds `Arc<P::Texture>` and reads the `vk::Image` + DRM
/// modifier through this trait without caring whether it's host or
/// consumer.
pub trait VulkanTextureLike {
    /// `vk::Image` handle, or `None` for placeholder textures.
    fn image(&self) -> Option<vk::Image>;
    /// DRM format modifier the host's driver chose at allocation time
    /// (consumer side: propagated from the host descriptor). Zero means
    /// `DRM_FORMAT_MOD_LINEAR` or "not applicable".
    fn chosen_drm_format_modifier(&self) -> u64;
}

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
