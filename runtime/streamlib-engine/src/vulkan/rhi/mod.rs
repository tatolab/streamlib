// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

mod command_recording_and_sync;
pub(crate) mod device;
mod frame_conversion;
mod kernels;
pub(crate) mod memory_and_buffers;
mod textures_and_readback;

pub(crate) use command_recording_and_sync::vulkan_command_buffer::VulkanCommandBuffer;
pub(crate) use command_recording_and_sync::vulkan_command_queue::VulkanCommandQueue;
pub(crate) use command_recording_and_sync::vulkan_queue_submission_completion::HostVulkanQueueSubmissionsSnapshot;
pub use device::host_marker::HostMarker;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use device::vulkan_device::PixelBufferUploadFinalTextureLayout;
pub use device::vulkan_device::{HostVulkanDevice, RayTracingPipelineProperties};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    unused_imports,
    reason = "the re-exports are the module's surface; not every one is used inside the crate"
)]
pub use command_recording_and_sync::vulkan_sync::HostVulkanTimelineSemaphore;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use memory_and_buffers::vulkan_upload_resources::HostVulkanUploadResources;
#[allow(
    unused_imports,
    reason = "the re-exports are the module's surface; not every one is used inside the crate"
)]
pub use textures_and_readback::vulkan_texture::HostVulkanTexture;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use streamlib_consumer_rhi::{
    VulkanRhiBuffer, VulkanRhiDevice, VulkanTextureLike, VulkanTimelineSemaphoreLike,
};

pub(crate) use textures_and_readback::vulkan_blitter::VulkanBlitter;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use frame_conversion::vulkan_tone_mapper::VulkanToneMapper;

pub use memory_and_buffers::vulkan_buffer::HostVulkanBuffer;

pub use memory_and_buffers::vulkan_storage_binding::{
    VulkanIndexBindable, VulkanStorageBindable, VulkanUniformBindable, VulkanVertexBindable,
};

pub use memory_and_buffers::vulkan_buffer_binding::VulkanBufferLike;

pub use command_recording_and_sync::vulkan_pipeline_flags::{VulkanAccess, VulkanStage};

pub(crate) use device::vulkan_validation_messenger::VulkanValidationMessageCounts;

#[cfg(target_os = "linux")]
pub(crate) use command_recording_and_sync::vulkan_command_recorder::RhiCommandRecorderInner;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use command_recording_and_sync::vulkan_command_recorder::{
    ImageCopyRegion, RhiCommandRecorder,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use frame_conversion::vulkan_offscreen_compositor::{
    OffscreenCompositorScalingMode, VulkanOffscreenCompositor,
};

pub(crate) use memory_and_buffers::vulkan_pixel_buffer_pool::VulkanPixelBufferPool;

pub use kernels::vulkan_compute_kernel::{ComputeKernelDispatch, VulkanComputeKernel};
pub(crate) use streamlib_consumer_rhi::SampledImageReadLayout;

pub use kernels::vulkan_graphics_kernel::{
    OffscreenColorTarget, OffscreenDraw, VulkanGraphicsKernel,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use kernels::vulkan_acceleration_structure::geometry_instance_flags_from_raw_bitmask;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use kernels::vulkan_acceleration_structure::{
    AccelerationStructureKind, IDENTITY_TRANSFORM, TlasInstanceDesc, VulkanAccelerationStructure,
};
pub use vulkanalia::vk::GeometryInstanceFlagsKHR;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use kernels::vulkan_ray_tracing_kernel::VulkanRayTracingKernel;

pub use textures_and_readback::vulkan_texture_readback::VulkanTextureReadback;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use frame_conversion::rgba_or_bgra_image_to_rgba_image_conversion::{
    ImageToImageColorConvertersWithTheirKernelsBuilt, LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN,
    TextureTheImageToImageKernelSamples, record_the_image_to_image_dispatch,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use frame_conversion::rgba_or_bgra_image_to_rgba_image_conversion::{
    TEXTURE_FORMATS_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_AS_STORED,
    texture_format_the_image_to_image_kernel_samples, the_image_to_image_kernel_samples_as_stored,
};
pub(crate) use frame_conversion::vulkan_color_converter::image_to_nv12_buffer_dispatch_group_counts;
pub(crate) use frame_conversion::vulkan_color_converter::{
    COLOR_CONVERTER_WORKGROUP_SIZE, VulkanColorConverter,
};

#[cfg(target_os = "macos")]
pub(crate) use memory_and_buffers::vulkan_imported_iosurface_storage_buffer::ImportedIOSurfaceStorageBuffer;

#[cfg(target_os = "linux")]
pub(crate) use memory_and_buffers::vulkan_host_mapping_read_by_gpu::HostMappingReadByGpu;
