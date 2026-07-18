// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Privileged engine-only RHI surface.
//!
//! In-tree surface adapters and engine-internal RHI code reach for
//! raw Vulkan handles through this module. The SDK-bucket types
//! ([`Texture`], [`PixelBufferRef`], [`GpuDevice`]) have no
//! inherent `vulkan_*` accessors — the only way to the privileged
//! surface is through the extension traits defined here. Importing
//! one of these traits is an explicit acknowledgment that the caller
//! is engine-side.
//!
//! Mirrors `streamlib-consumer-rhi`'s carve-out for cdylibs (#560):
//! the FullAccess capability boundary is enforced by the Cargo dep
//! graph, not by convention. Per CLAUDE.md "type-system enforcement
//! beats convention".
//!
//! Post-#731 (SDK extraction), this module moves to `streamlib-engine`
//! and consumer call sites flip `use streamlib::sdk::engine::Host*Ext;` to
//! `use streamlib_engine::Host*Ext;` — same shape, new path.
//!
//! # Boundary lock
//!
//! Without the extension trait in scope, the privileged accessors are
//! unreachable from the SDK-bucket types' inherent impls. This snippet
//! must fail to compile:
//!
//! ```compile_fail
//! # #[cfg(target_os = "linux")]
//! # fn _check(stream_texture: &streamlib::sdk::rhi::Texture) {
//! // Without `use streamlib::sdk::engine::HostTextureExt;` the privileged
//! // `vulkan_inner` accessor is not visible — boundary held.
//! let _ = stream_texture.vulkan_inner();
//! # }
//! ```
//!
//! With the trait imported, the same call type-checks:
//!
//! ```no_run
//! # #[cfg(target_os = "linux")]
//! # fn _check(stream_texture: &streamlib::sdk::rhi::Texture) {
//! use streamlib::sdk::engine::HostTextureExt;
//! let _ = stream_texture.vulkan_inner();
//! # }
//! ```
//!
//! [`Texture`]: crate::core::rhi::Texture
//! [`PixelBufferRef`]: crate::core::rhi::PixelBufferRef
//! [`GpuDevice`]: crate::core::rhi::GpuDevice

use std::sync::Arc;

pub use crate::vulkan::rhi::{
    AccelerationStructureKind, HostMarker, HostVulkanBuffer, HostVulkanDevice, HostVulkanTexture,
    HostVulkanTimelineSemaphore, IDENTITY_TRANSFORM, ImageCopyRegion, OffscreenColorTarget,
    OffscreenDraw, RayTracingPipelineProperties, RhiCommandRecorder, ThirdPartyGpuCapabilities,
    TlasInstanceDesc, VulkanAccelerationStructure, VulkanAccess, VulkanBufferLike,
    VulkanComputeKernel, VulkanGraphicsKernel, VulkanIndexBindable, VulkanRayTracingKernel,
    VulkanStage, VulkanStorageBindable, VulkanTextureReadback, VulkanUniformBindable,
    VulkanVertexBindable, drm_modifier_probe,
};

#[cfg(target_os = "linux")]
pub use crate::vulkan::rhi::{MAX_FRAMES_IN_FLIGHT, PresentFrame, VulkanPresentTarget};

pub use vulkanalia::vk::GeometryInstanceFlagsKHR;

use crate::core::error::{Error, Result};
use crate::core::rhi::texture::TextureInner;
use crate::core::rhi::{GpuDevice, PixelBufferRef, Texture};

/// Privileged engine-side accessors for [`Texture`].
///
/// Engine RHI helpers and in-tree adapters import this trait to wrap a
/// freshly-allocated [`HostVulkanTexture`] as a [`Texture`] and
/// to reach the underlying handle for raw `VkImage` access. Customer
/// code never imports this trait — `streamlib::sdk::rhi::Texture`
/// is opaque on its public inherent impl.
///
/// [`HostVulkanTexture`]: crate::vulkan::rhi::HostVulkanTexture
pub trait HostTextureExt {
    /// Wrap an already-allocated [`HostVulkanTexture`] as a
    /// [`Texture`].
    fn from_vulkan(texture: HostVulkanTexture) -> Self;

    /// Borrow the underlying [`HostVulkanTexture`] for raw `VkImage`
    /// access, DRM-modifier introspection, and adapter-side layout
    /// transitions. **Host-only** — panics in cdylib mode through
    /// `Texture::host_inner()`.
    fn vulkan_inner(&self) -> &Arc<HostVulkanTexture>;

    /// Clone the underlying `Arc<HostVulkanTexture>` and hand back an
    /// owned strong count. Engine-internal host-mode accessor for
    /// in-tree RHI helpers (texture readback, tone mapper) that need
    /// an owned `Arc<HostVulkanTexture>` from a `Texture`. No plugin
    /// ABI transit slot backs it — cdylib GPU code reads a texture
    /// through the cdylib-safe getters, never the raw Arc.
    fn host_vulkan_texture_arc(&self) -> Result<Arc<HostVulkanTexture>>;
}

impl HostTextureExt for Texture {
    fn from_vulkan(texture: HostVulkanTexture) -> Self {
        let inner = TextureInner {
            inner: Arc::new(texture),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            metal_texture: None,
        };
        Texture::from_inner(inner)
    }

    fn vulkan_inner(&self) -> &Arc<HostVulkanTexture> {
        &self.host_inner().inner
    }

    fn host_vulkan_texture_arc(&self) -> Result<Arc<HostVulkanTexture>> {
        if crate::core::plugin::host_services::host_callbacks().is_none() {
            return Ok(Arc::clone(self.vulkan_inner()));
        }
        Err(Error::GpuError(
            "host_vulkan_texture_arc: engine-internal host-mode accessor; the \
             raw-texture plugin ABI transit slot was removed (#1270). Cdylib \
             GPU code reads a texture through the cdylib-safe Texture getters \
             and FullAccess primitives, never the raw Arc."
                .into(),
        ))
    }
}

/// Privileged engine-side accessor for [`PixelBufferRef`].
///
/// In-tree adapters that issue `vkCmdCopyImageToBuffer` or
/// `vkCmdCopyBufferToImage` against a HOST_VISIBLE staging buffer
/// reach the underlying [`HostVulkanBuffer`] through this trait.
///
/// [`HostVulkanBuffer`]: crate::vulkan::rhi::HostVulkanBuffer
pub trait HostPixelBufferRefExt {
    /// Borrow the underlying [`HostVulkanBuffer`].
    fn vulkan_inner(&self) -> &Arc<HostVulkanBuffer>;
}

impl HostPixelBufferRefExt for PixelBufferRef {
    fn vulkan_inner(&self) -> &Arc<HostVulkanBuffer> {
        &self.inner
    }
}

/// Privileged engine-side accessor for [`GpuDevice`].
///
/// Engine RHI helpers and in-tree adapters reach the underlying
/// [`HostVulkanDevice`] for raw queue / command-pool / submit access.
///
/// [`HostVulkanDevice`]: crate::vulkan::rhi::HostVulkanDevice
pub trait HostGpuDeviceExt {
    /// Borrow the underlying [`HostVulkanDevice`].
    fn vulkan_device(&self) -> &Arc<HostVulkanDevice>;
}

impl HostGpuDeviceExt for GpuDevice {
    fn vulkan_device(&self) -> &Arc<HostVulkanDevice> {
        &self.inner
    }
}

/// Privileged engine-side accessors for [`SurfaceStore`] surface
/// registration paths whose parameter types are host-internal.
///
/// `register_texture` and `register_pixel_buffer_with_timeline` both
/// take `Option<&HostVulkanTimelineSemaphore>` — timeline-semaphore
/// construction is FullAccess-privileged and the type is host-
/// internal by construction, so these methods are unreachable from
/// cdylib code through typed Rust. The extension-trait pattern
/// mirrors [`HostTextureExt`] / [`HostPixelBufferRefExt`] /
/// [`HostGpuDeviceExt`] to lock the engine-only contract at the
/// type-system layer rather than by convention.
///
/// Cdylib subprocess customers reach surfaces by `surface_id`
/// lookup (`lookup_texture` / `check_out`), not by re-registering;
/// the dual-registration shape documented in
/// `docs/architecture/adapter-runtime-integration.md` is host-side
/// `install_setup_hook` plumbing only.
///
/// # Boundary lock
///
/// Without the trait in scope, the privileged accessors are
/// unreachable from the [`SurfaceStore`] PluginAbiObject's inherent impl:
///
/// ```compile_fail
/// # #[cfg(target_os = "linux")]
/// # fn _check(
/// #     store: &streamlib::sdk::context::SurfaceStore,
/// #     texture: &streamlib::sdk::rhi::Texture,
/// # ) -> Result<(), Box<dyn std::error::Error>> {
/// // Without `use streamlib::sdk::engine::HostSurfaceStoreExt;`
/// // `register_texture` is not visible — boundary held.
/// store.register_texture(
///     "id",
///     texture,
///     None,
///     None,
///     streamlib::sdk::rhi::VulkanLayout::UNDEFINED,
/// )?;
/// # Ok(())
/// # }
/// ```
///
/// [`SurfaceStore`]: crate::core::context::SurfaceStore
#[cfg(target_os = "linux")]
pub trait HostSurfaceStoreExt {
    /// Register a texture for cross-process sharing with the
    /// single-writer-per-edge timeline pair (`produce_done` signaled
    /// by the producer, `consume_done` signaled by the consumer).
    /// See `docs/architecture/adapter-timeline-single-writer.md` for
    /// the contract.
    fn register_texture(
        &self,
        surface_id: &str,
        texture: &Texture,
        produce_done: Option<&HostVulkanTimelineSemaphore>,
        consume_done: Option<&HostVulkanTimelineSemaphore>,
        current_image_layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> crate::core::error::Result<()>;

    /// Register a pixel buffer with the single-writer-per-edge
    /// timeline pair (host-side adapter `install_setup_hook` path).
    fn register_pixel_buffer_with_timeline(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        produce_done: Option<&HostVulkanTimelineSemaphore>,
        consume_done: Option<&HostVulkanTimelineSemaphore>,
    ) -> crate::core::error::Result<()>;
}

#[cfg(target_os = "linux")]
impl HostSurfaceStoreExt for crate::core::context::SurfaceStore {
    fn register_texture(
        &self,
        surface_id: &str,
        texture: &Texture,
        produce_done: Option<&HostVulkanTimelineSemaphore>,
        consume_done: Option<&HostVulkanTimelineSemaphore>,
        current_image_layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> crate::core::error::Result<()> {
        self.host_register_texture(
            surface_id,
            texture,
            produce_done,
            consume_done,
            current_image_layout,
        )
    }

    fn register_pixel_buffer_with_timeline(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        produce_done: Option<&HostVulkanTimelineSemaphore>,
        consume_done: Option<&HostVulkanTimelineSemaphore>,
    ) -> crate::core::error::Result<()> {
        self.host_register_pixel_buffer_with_timeline(
            surface_id,
            pixel_buffer,
            produce_done,
            consume_done,
        )
    }
}
