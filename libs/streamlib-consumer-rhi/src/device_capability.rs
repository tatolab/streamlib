// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Capability markers and the small device-shape trait surface adapters
//! abstract over.
//!
//! [`HostMarker`] and [`ConsumerMarker`] are the privilege flavors a
//! Vulkan-typed RHI resource can be parameterized on. They feed into
//! the source-level split (`HostVulkanDevice` / `ConsumerVulkanDevice`
//! and the parametric texture / pixel-buffer / timeline-semaphore
//! types) so the FullAccess capability boundary is enforced by the
//! type system, not by convention.
//!
//! `ConsumerMarker` and the consumer-flavored `DevicePrivilege` impl
//! live in this crate. `HostMarker` lives in `streamlib::vulkan::rhi`
//! alongside the host RHI types it points at — the impl needs to name
//! `HostVulkanTexture`, which is a streamlib-side type, so the
//! orphan-rule-compatible home is the streamlib crate.
//!
//! [`VulkanRhiDevice`] is the minimal set of methods every surface
//! adapter needs at its layout-transition seam: a logical device, the
//! queue it submits to, that queue's family index, and a
//! mutex-protected submit. Both `HostVulkanDevice` and
//! `ConsumerVulkanDevice` implement it; adapter code is generic over
//! `D: VulkanRhiDevice<Privilege = …>` and works against either.

use vulkanalia::vk;

use crate::Result;

/// Privilege marker for consumer-side Vulkan resources — carve-out
/// only. See `docs/architecture/subprocess-rhi-parity.md`: DMA-BUF FD
/// import + bind + map, tiled-image import via
/// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`, layout transitions on
/// imported handles, sync wait/signal on imported timeline semaphores.
pub struct ConsumerMarker;

/// Trait restricting privilege markers to the streamlib-defined
/// `HostMarker` and the consumer-rhi-defined [`ConsumerMarker`].
///
/// Carries the timeline-semaphore + texture associated types so
/// adapter code that holds `Arc<P::TimelineSemaphore>` /
/// `Arc<P::Texture>` resolves to the right concrete types at
/// instantiation. Both flavors implement the corresponding `*Like`
/// trait so the adapter can call `wait` / `signal_host` / `image`
/// without knowing which side it's on.
///
/// External crates SHOULD NOT implement this trait — extending the
/// privilege ladder requires coordinated changes across both
/// `streamlib-consumer-rhi` and `streamlib`.
pub trait DevicePrivilege: 'static + Send + Sync {
    /// Concrete timeline-semaphore type for this privilege flavor.
    type TimelineSemaphore: VulkanTimelineSemaphoreLike + Send + Sync + 'static;
    /// Concrete texture type for this privilege flavor.
    type Texture: VulkanTextureLike + Send + Sync + 'static;
}

impl DevicePrivilege for ConsumerMarker {
    type TimelineSemaphore = super::ConsumerVulkanTimelineSemaphore;
    type Texture = super::ConsumerVulkanTexture;
}

/// Operations the surface adapter needs from a timeline semaphore.
/// Both [`crate::ConsumerVulkanTimelineSemaphore`] and
/// `streamlib::vulkan::rhi::HostVulkanTimelineSemaphore` implement this
/// — the trait exists so `VulkanSurfaceAdapter` can be generic over the
/// device flavor without dynamic dispatch.
pub trait VulkanTimelineSemaphoreLike {
    /// Block until the timeline counter reaches or exceeds `value`.
    fn wait(&self, value: u64, timeout_ns: u64) -> Result<()>;
    /// Host-side signal: advance the counter to `value`.
    fn signal_host(&self, value: u64) -> Result<()>;
}

/// Operations the surface adapter needs from a Vulkan-flavored
/// texture. Both [`crate::ConsumerVulkanTexture`] and
/// `streamlib::vulkan::rhi::HostVulkanTexture` implement this — the
/// adapter holds `Arc<P::Texture>` and reads the `vk::Image` +
/// metadata through this trait without caring whether it's host or
/// consumer.
pub trait VulkanTextureLike {
    /// `vk::Image` handle, or `None` for placeholder textures.
    fn image(&self) -> Option<vk::Image>;
    /// DRM format modifier the host's driver chose at allocation time
    /// (consumer side: propagated from the host descriptor). Zero
    /// means `DRM_FORMAT_MOD_LINEAR` or "not applicable".
    fn chosen_drm_format_modifier(&self) -> u64;
    /// Texture width in pixels.
    fn width(&self) -> u32;
    /// Texture height in pixels.
    fn height(&self) -> u32;
    /// Texture format.
    fn format(&self) -> crate::TextureFormat;
}

/// Minimal device shape every surface adapter needs at the layout-
/// transition + submit seam, plus the raw-handle accessors the
/// `raw_handles()` escape hatch surfaces to power-user customers.
///
/// Implementations expose their raw `vulkanalia::Device` so adapters
/// can record command buffers, but capability enforcement happens at
/// the **crate boundary**: a cdylib that depends only on
/// `streamlib-consumer-rhi` cannot reach `HostVulkanDevice` at all,
/// only `ConsumerVulkanDevice`. Both flavors expose the same shape;
/// what differs is the privileged constructors and the queue matrix
/// behind them.
pub trait VulkanRhiDevice: Send + Sync {
    /// Privilege marker — `HostMarker` or [`ConsumerMarker`].
    type Privilege: DevicePrivilege;

    /// Underlying Vulkan instance. Power-user customers (cdylibs,
    /// 3rd-party adapters) cast this to their binding's instance type
    /// via `as_raw()`.
    fn instance(&self) -> &vulkanalia::Instance;

    /// Selected physical device.
    fn physical_device(&self) -> vk::PhysicalDevice;

    /// Logical Vulkan device.
    fn device(&self) -> &vulkanalia::Device;

    /// Default submit queue.
    fn queue(&self) -> vk::Queue;

    /// Queue family that owns [`Self::queue`].
    fn queue_family_index(&self) -> u32;

    /// Submit command buffers under the device's per-queue mutex.
    /// Vulkan requires external synchronization for `vkQueueSubmit2`
    /// against the same `VkQueue` from multiple threads; the host's
    /// per-queue mutex is the canonical way streamlib serializes
    /// that. Consumer-side devices can use a single mutex since they
    /// only hold one queue.
    ///
    /// # Safety
    /// Caller must satisfy the standard `vkQueueSubmit2`
    /// preconditions (valid `submits`, valid optional `fence`, no
    /// concurrent native submits to `queue` outside this trait).
    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()>;
}
