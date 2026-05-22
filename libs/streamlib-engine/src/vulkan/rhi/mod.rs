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
#[cfg(target_os = "linux")]
mod vulkan_upload_resources;

pub use host_marker::HostMarker;
pub use vulkan_command_buffer::VulkanCommandBuffer;
pub use vulkan_command_queue::VulkanCommandQueue;
pub use vulkan_device::{HostVulkanDevice, RayTracingPipelineProperties, ThirdPartyGpuCapabilities};
#[allow(unused_imports)]
pub use vulkan_sync::{VulkanFence, VulkanSemaphore};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use vulkan_sync::HostVulkanTimelineSemaphore;
pub use vulkan_texture::HostVulkanTexture;
#[cfg(target_os = "linux")]
pub use vulkan_upload_resources::HostVulkanUploadResources;

// Trait machinery + Consumer flavor — re-exported from the canonical
// home in `streamlib-consumer-rhi`. Some entries appear unused inside
// streamlib itself (callers reach the trait machinery via streamlib's
// re-export); the `#[allow(unused_imports)]` keeps the surface
// available for downstream crates that still pull these names through
// `streamlib::vulkan::rhi`.
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use streamlib_consumer_rhi::{
    ConsumerMarker, ConsumerVulkanBuffer, ConsumerVulkanDevice, ConsumerVulkanTexture,
    ConsumerVulkanTimelineSemaphore, DevicePrivilege, VulkanRhiBuffer, VulkanRhiDevice,
    VulkanTextureLike, VulkanTimelineSemaphoreLike,
};

mod vulkan_blitter;
pub use vulkan_blitter::VulkanBlitter;

#[cfg(target_os = "linux")]
mod vulkan_tone_mapper;
#[cfg(target_os = "linux")]
pub use vulkan_tone_mapper::{VulkanToneMapper, TONE_MAPPER_WORKGROUP_SIZE};

pub(crate) mod vulkan_buffer;
pub use vulkan_buffer::HostVulkanBuffer;

mod vulkan_storage_binding;
pub use vulkan_storage_binding::{
    VulkanIndexBindable, VulkanStorageBindable, VulkanUniformBindable, VulkanVertexBindable,
};

mod vulkan_buffer_binding;
pub use vulkan_buffer_binding::VulkanBufferLike;

mod vulkan_pipeline_flags;
pub use vulkan_pipeline_flags::{VulkanAccess, VulkanStage};

#[cfg(target_os = "linux")]
mod vulkan_command_recorder;
#[cfg(target_os = "linux")]
pub use vulkan_command_recorder::{ImageCopyRegion, RhiCommandRecorder};
pub(crate) use vulkan_compute_kernel::VulkanComputeKernelInner;
pub(crate) use vulkan_graphics_kernel::VulkanGraphicsKernelInner;
pub(crate) use vulkan_ray_tracing_kernel::VulkanRayTracingKernelInner;
// `RhiCommandRecorderInner` is needed by `core::plugin::host_services`
// for `Box::from_raw` in `drop_command_recorder`. Crate-scope export.
pub(crate) use vulkan_command_recorder::RhiCommandRecorderInner;

#[cfg(target_os = "linux")]
mod vulkan_present_target;
#[cfg(target_os = "linux")]
pub use vulkan_present_target::{
    PresentFrame, VulkanPresentTarget, MAX_FRAMES_IN_FLIGHT,
};

#[cfg(target_os = "linux")]
mod vulkan_swapchain_colorspace;
#[cfg(target_os = "linux")]
pub use vulkan_swapchain_colorspace::{
    build_hdr_metadata, pick_swapchain_format, SwapchainColorPick,
};

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
// `VulkanAccelerationStructureInner` is `pub(crate)`-shaped — only
// the host's clone/drop callbacks in `core::plugin::host_services`
// need to reference it for `Arc::increment_strong_count` /
// `Arc::decrement_strong_count`. Re-export at crate scope.
#[cfg(target_os = "linux")]
pub(crate) use vulkan_acceleration_structure::VulkanAccelerationStructureInner;

#[cfg(target_os = "linux")]
mod vulkan_ray_tracing_kernel;
#[cfg(target_os = "linux")]
pub use vulkan_ray_tracing_kernel::VulkanRayTracingKernel;

mod vulkan_texture_readback;
pub use vulkan_texture_readback::VulkanTextureReadback;

mod vulkan_color_converter;
pub use vulkan_color_converter::VulkanColorConverter;

#[cfg(target_os = "linux")]
pub mod drm_modifier_probe;

#[cfg(all(test, target_os = "linux"))]
mod vulkan_swapchain_alloc_repro_test;
