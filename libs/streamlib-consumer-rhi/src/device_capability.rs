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

/// Sealing supertrait for [`DevicePrivilege`]. Lives in a `#[doc(hidden)]`
/// module so streamlib can implement it on `HostMarker` (the seal needs
/// to span the two crates because `HostMarker` carries
/// streamlib-side associated types and so cannot live in this crate),
/// while still preventing external implementations of
/// [`DevicePrivilege`] from outside the workspace.
#[doc(hidden)]
pub mod private {
    /// Sealing trait — see [`super::DevicePrivilege`]. Public so
    /// streamlib can `impl Sealed for HostMarker`, but doc-hidden so
    /// it doesn't appear in the public API.
    pub trait Sealed {}
}

/// Privilege marker for consumer-side Vulkan resources — carve-out
/// only. See `docs/architecture/subprocess-rhi-parity.md`: DMA-BUF FD
/// import + bind + map, tiled-image import via
/// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`, layout transitions on
/// imported handles, sync wait/signal on imported timeline semaphores.
pub struct ConsumerMarker;
impl private::Sealed for ConsumerMarker {}

/// Sealed trait restricting privilege markers to the
/// streamlib-defined `HostMarker` and the consumer-rhi-defined
/// [`ConsumerMarker`].
///
/// Carries the timeline-semaphore + texture associated types so
/// adapter code that holds `Arc<P::TimelineSemaphore>` /
/// `Arc<P::Texture>` resolves to the right concrete types at
/// instantiation. Both flavors implement the corresponding `*Like`
/// trait so the adapter can call `wait` / `signal_host` / `image`
/// without knowing which side it's on.
///
/// The `private::Sealed` supertrait keeps this trait closed to the
/// two markers shipped by the workspace; external crates can name
/// the type but cannot implement it. Extending the privilege ladder
/// requires coordinated changes across both `streamlib-consumer-rhi`
/// and `streamlib`.
pub trait DevicePrivilege: private::Sealed + 'static + Send + Sync {
    /// Concrete timeline-semaphore type for this privilege flavor.
    type TimelineSemaphore: VulkanTimelineSemaphoreLike + Send + Sync + 'static;
    /// Concrete texture type for this privilege flavor.
    type Texture: VulkanTextureLike + Send + Sync + 'static;
    /// Concrete HOST_VISIBLE staging-buffer type for this privilege
    /// flavor. Adapters that need CPU-mapped per-plane staging
    /// (cpu-readback) hold `Arc<P::PixelBuffer>` and read its mapped
    /// pointer through [`VulkanPixelBufferLike`].
    type PixelBuffer: VulkanPixelBufferLike + Send + Sync + 'static;
}

impl DevicePrivilege for ConsumerMarker {
    type TimelineSemaphore = super::ConsumerVulkanTimelineSemaphore;
    type Texture = super::ConsumerVulkanTexture;
    type PixelBuffer = super::ConsumerVulkanPixelBuffer;
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
    /// Raw `vk::Semaphore` handle for inclusion in
    /// `VkSubmitInfo2::pSignalSemaphoreInfos` /
    /// `pWaitSemaphoreInfos`. Surfaces that need to schedule
    /// timeline-coupled work on a queue (cpu-readback's host trigger,
    /// vulkan adapter's submit-with-signal flows) reach the raw handle
    /// through this method so they don't need to know the concrete
    /// privilege flavor.
    fn semaphore(&self) -> vk::Semaphore;
}

/// Operations the surface adapter needs from a HOST_VISIBLE staging
/// `VkBuffer`. Both [`crate::ConsumerVulkanPixelBuffer`] and
/// `streamlib::vulkan::rhi::HostVulkanPixelBuffer` implement this —
/// cpu-readback adapter code holds `Arc<P::PixelBuffer>` and reads
/// the `vk::Buffer` handle plus the mapped pointer through this
/// trait without caring whether it's host or consumer.
///
/// On the host side the underlying buffer is allocated through
/// [`HostVulkanPixelBuffer::new`] (HOST_VISIBLE / HOST_COHERENT linear
/// `VkBuffer` exported as a DMA-BUF). On the consumer side it is
/// imported from the host's exported FD via
/// [`crate::ConsumerVulkanPixelBuffer::from_dma_buf_fd`]. Both expose
/// the same `vk::Buffer` + mapped-pointer + plane-size shape.
pub trait VulkanPixelBufferLike {
    /// `vk::Buffer` handle for plane 0.
    fn buffer(&self) -> vk::Buffer;
    /// Persistently mapped CPU pointer for plane 0.
    fn mapped_ptr(&self) -> *mut u8;
    /// Plane 0 size in bytes.
    fn size(&self) -> vk::DeviceSize;
    /// Buffer width in pixels.
    fn width(&self) -> u32;
    /// Buffer height in pixels.
    fn height(&self) -> u32;
    /// Bytes per pixel.
    fn bytes_per_pixel(&self) -> u32;
}

/// Operations the surface adapter needs from a Vulkan-flavored
/// texture. Both [`crate::ConsumerVulkanTexture`] and
/// `streamlib::vulkan::rhi::HostVulkanTexture` implement this — the
/// adapter holds `Arc<P::Texture>` and reads the `vk::Image` +
/// metadata through this trait without caring whether it's host or
/// consumer.
///
/// The trait is split into two groups:
///
/// - **Identity / shape** ([`Self::image`], [`Self::format`],
///   [`Self::width`], [`Self::height`], [`Self::chosen_drm_format_modifier`]):
///   the original surface-adapter v1 surface — what an adapter needs
///   to record a layout transition on the right `VkImage` of the right
///   shape.
/// - **Full image metadata** ([`Self::vk_format`] through
///   [`Self::vk_ycbcr_conversion_handle`]): what frameworks like Skia
///   need to wrap an externally-allocated `VkImage` as their native
///   surface type (`GrVkBackendContext` → `GrBackendRenderTarget` →
///   `SkSurface`). Defaults match the streamlib surface-adapter
///   model: DEVICE_LOCAL, single-sample, single-mip, unprotected, no
///   YCbCr conversion, bound at offset 0. Constructors that violate
///   any default override the corresponding method.
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

    /// Vulkan format the image was created with. Distinct from
    /// [`Self::format`] (the RHI-level enum) — this is the raw
    /// `vk::Format` the framework consumer sees. Required because
    /// Skia rejects `VK_FORMAT_UNDEFINED`.
    fn vk_format(&self) -> vk::Format;

    /// `VkImageTiling` used at image creation. `OPTIMAL` for the
    /// standard render-target path; `DRM_FORMAT_MODIFIER_EXT` for
    /// tiled DMA-BUF render targets.
    fn vk_image_tiling(&self) -> vk::ImageTiling;

    /// `VkImageUsageFlags` the image was created with. Skia checks
    /// these bits to decide which surface ops are valid (e.g.
    /// `COLOR_ATTACHMENT` is required for `wrap_backend_render_target`).
    fn vk_image_usage_flags(&self) -> vk::ImageUsageFlags;

    /// `VkDeviceMemory` handle the image is bound to. Returns
    /// `vk::DeviceMemory::null()` for placeholder textures.
    fn vk_memory(&self) -> vk::DeviceMemory;

    /// Byte size of the memory allocation backing the image.
    fn vk_memory_size(&self) -> vk::DeviceSize;

    /// `VkSampleCountFlagBits` the image was created with. Default
    /// `_1` covers every surface-adapter-managed texture today;
    /// multi-sample render targets aren't part of the surface-adapter
    /// contract.
    fn vk_sample_count(&self) -> vk::SampleCountFlags {
        vk::SampleCountFlags::_1
    }

    /// Number of mip levels. Default `1` — surface-adapter textures
    /// don't carry mipmaps.
    fn vk_level_count(&self) -> u32 {
        1
    }

    /// Byte offset of the image's storage within [`Self::vk_memory`].
    /// Default `0` — surface-adapter textures bind at offset 0
    /// (VMA dedicated allocations and DMA-BUF imports both).
    fn vk_memory_offset(&self) -> vk::DeviceSize {
        0
    }

    /// `VkMemoryPropertyFlags` of the backing allocation. Default
    /// `DEVICE_LOCAL` matches the universal surface-adapter shape
    /// (render targets are GPU-local; HOST_VISIBLE staging buffers
    /// live on [`VulkanPixelBufferLike`], not here).
    fn vk_memory_property_flags(&self) -> vk::MemoryPropertyFlags {
        vk::MemoryPropertyFlags::DEVICE_LOCAL
    }

    /// `true` if the image was allocated with
    /// `VK_IMAGE_CREATE_PROTECTED_BIT`. Default `false` — surface-
    /// adapter textures aren't protected-content.
    fn vk_protected(&self) -> bool {
        false
    }

    /// `VkSamplerYcbcrConversion` handle as `u64`, or `0` if unused.
    /// Default `0` — adapter-internal NV12 conversion is the
    /// surface-adapter contract; YCbCr conversion handles aren't
    /// exposed across the boundary.
    fn vk_ycbcr_conversion_handle(&self) -> u64 {
        0
    }
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
