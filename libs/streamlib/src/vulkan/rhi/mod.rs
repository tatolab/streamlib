// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan RHI implementation.
//!
//! Device, texture, command buffer/queue, sync, pixel buffer, and texture cache
//! are fully implemented via ash. Blitter and format converter are partial.
//!
//! The `Consumer*` types and the `VulkanRhiDevice` / `DevicePrivilege`
//! / `VulkanTextureLike` / `VulkanTimelineSemaphoreLike` trait
//! machinery live in [`streamlib_consumer_rhi`]. They are re-exported
//! here so existing in-tree call sites compile unchanged. Subprocess
//! cdylibs depend on `streamlib-consumer-rhi` directly so the
//! FullAccess capability boundary is enforced by the type system.

mod host_marker;
mod vulkan_command_buffer;
mod vulkan_command_queue;
mod vulkan_device;
mod vulkan_sync;
mod vulkan_texture;

pub use host_marker::HostMarker;
pub use vulkan_command_buffer::VulkanCommandBuffer;
pub use vulkan_command_queue::VulkanCommandQueue;
pub use vulkan_device::{HostVulkanDevice, RayTracingPipelineProperties};
#[allow(unused_imports)]
pub use vulkan_sync::{VulkanFence, VulkanSemaphore};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use vulkan_sync::HostVulkanTimelineSemaphore;
pub use vulkan_texture::HostVulkanTexture;

// Trait machinery + Consumer flavor — re-exported from the canonical
// home in `streamlib-consumer-rhi`. Some entries appear unused inside
// streamlib itself (callers reach the trait machinery via streamlib's
// re-export); the `#[allow(unused_imports)]` keeps the surface
// available for downstream crates that still pull these names through
// `streamlib::vulkan::rhi`.
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use streamlib_consumer_rhi::{
    ConsumerMarker, ConsumerVulkanDevice, ConsumerVulkanPixelBuffer, ConsumerVulkanTexture,
    ConsumerVulkanTimelineSemaphore, DevicePrivilege, VulkanPixelBufferLike, VulkanRhiDevice,
    VulkanTextureLike, VulkanTimelineSemaphoreLike,
};

mod vulkan_blitter;
pub use vulkan_blitter::VulkanBlitter;

pub(crate) mod vulkan_pixel_buffer;
pub use vulkan_pixel_buffer::HostVulkanPixelBuffer;

mod vulkan_texture_cache;
pub use vulkan_texture_cache::VulkanTextureCache;

mod vulkan_pixel_buffer_pool;
pub use vulkan_pixel_buffer_pool::VulkanPixelBufferPool;

mod vulkan_compute_kernel;
pub use vulkan_compute_kernel::VulkanComputeKernel;

mod vulkan_graphics_kernel;
pub use vulkan_graphics_kernel::{
    OffscreenColorTarget, OffscreenDraw, VulkanGraphicsKernel,
};

#[cfg(target_os = "linux")]
mod vulkan_acceleration_structure;
#[cfg(target_os = "linux")]
pub use vulkan_acceleration_structure::{
    AccelerationStructureKind, TlasInstanceDesc, VulkanAccelerationStructure, IDENTITY_TRANSFORM,
};

#[cfg(target_os = "linux")]
mod vulkan_ray_tracing_kernel;
#[cfg(target_os = "linux")]
pub use vulkan_ray_tracing_kernel::VulkanRayTracingKernel;

mod vulkan_texture_readback;
pub use vulkan_texture_readback::VulkanTextureReadback;

mod vulkan_format_converter;
pub use vulkan_format_converter::VulkanFormatConverter;

mod vulkan_blending_compositor;
pub use vulkan_blending_compositor::{
    flag_bits as blending_compositor_flags, BlendingCompositorInputs,
    BlendingCompositorPushConstants, VulkanBlendingCompositor,
};

mod vulkan_crt_film_grain;
pub use vulkan_crt_film_grain::{
    CrtFilmGrainInputs, CrtFilmGrainPushConstants, VulkanCrtFilmGrain,
};

#[cfg(target_os = "linux")]
pub mod drm_modifier_probe;

#[cfg(all(test, target_os = "linux"))]
mod vulkan_swapchain_alloc_repro_test;
