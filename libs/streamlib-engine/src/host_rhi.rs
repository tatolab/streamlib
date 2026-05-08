// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Privileged engine-only RHI surface.
//!
//! In-tree surface adapters and engine-internal RHI code reach for
//! raw Vulkan handles through this module. The SDK-bucket types
//! ([`StreamTexture`], [`RhiPixelBufferRef`], [`GpuDevice`]) have no
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
//! # fn _check(stream_texture: &streamlib::sdk::rhi::StreamTexture) {
//! // Without `use streamlib::sdk::engine::HostStreamTextureExt;` the privileged
//! // `vulkan_inner` accessor is not visible — boundary held.
//! let _ = stream_texture.vulkan_inner();
//! # }
//! ```
//!
//! With the trait imported, the same call type-checks:
//!
//! ```no_run
//! # #[cfg(target_os = "linux")]
//! # fn _check(stream_texture: &streamlib::sdk::rhi::StreamTexture) {
//! use streamlib::sdk::engine::HostStreamTextureExt;
//! let _ = stream_texture.vulkan_inner();
//! # }
//! ```
//!
//! [`StreamTexture`]: crate::core::rhi::StreamTexture
//! [`RhiPixelBufferRef`]: crate::core::rhi::RhiPixelBufferRef
//! [`GpuDevice`]: crate::core::rhi::GpuDevice

use std::sync::Arc;

pub use crate::vulkan::rhi::{
    drm_modifier_probe, AccelerationStructureKind, HostMarker, HostVulkanDevice,
    HostVulkanPixelBuffer, HostVulkanTexture, HostVulkanTimelineSemaphore, OffscreenColorTarget,
    OffscreenDraw, RayTracingPipelineProperties, TlasInstanceDesc, VulkanAccelerationStructure,
    VulkanComputeKernel, VulkanGraphicsKernel, VulkanRayTracingKernel, VulkanTextureReadback,
    IDENTITY_TRANSFORM,
};

pub use vulkanalia::vk::GeometryInstanceFlagsKHR;

use crate::core::rhi::{GpuDevice, RhiPixelBufferRef, StreamTexture};

/// Privileged engine-side accessors for [`StreamTexture`].
///
/// Engine RHI helpers and in-tree adapters import this trait to wrap a
/// freshly-allocated [`HostVulkanTexture`] as a [`StreamTexture`] and
/// to reach the underlying handle for raw `VkImage` access. Customer
/// code never imports this trait — `streamlib::sdk::rhi::StreamTexture`
/// is opaque on its public inherent impl.
///
/// [`HostVulkanTexture`]: crate::vulkan::rhi::HostVulkanTexture
pub trait HostStreamTextureExt {
    /// Wrap an already-allocated [`HostVulkanTexture`] as a
    /// [`StreamTexture`].
    fn from_vulkan(texture: HostVulkanTexture) -> Self;

    /// Borrow the underlying [`HostVulkanTexture`] for raw `VkImage`
    /// access, DRM-modifier introspection, and adapter-side layout
    /// transitions.
    fn vulkan_inner(&self) -> &Arc<HostVulkanTexture>;
}

impl HostStreamTextureExt for StreamTexture {
    fn from_vulkan(texture: HostVulkanTexture) -> Self {
        StreamTexture {
            inner: Arc::new(texture),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            metal_texture: None,
        }
    }

    fn vulkan_inner(&self) -> &Arc<HostVulkanTexture> {
        &self.inner
    }
}

/// Privileged engine-side accessor for [`RhiPixelBufferRef`].
///
/// In-tree adapters that issue `vkCmdCopyImageToBuffer` or
/// `vkCmdCopyBufferToImage` against a HOST_VISIBLE staging buffer
/// reach the underlying [`HostVulkanPixelBuffer`] through this trait.
///
/// [`HostVulkanPixelBuffer`]: crate::vulkan::rhi::HostVulkanPixelBuffer
pub trait HostRhiPixelBufferRefExt {
    /// Borrow the underlying [`HostVulkanPixelBuffer`].
    fn vulkan_inner(&self) -> &Arc<HostVulkanPixelBuffer>;
}

impl HostRhiPixelBufferRefExt for RhiPixelBufferRef {
    fn vulkan_inner(&self) -> &Arc<HostVulkanPixelBuffer> {
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
