// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan RHI implementation.
//!
//! Device, texture, command buffer/queue, sync, pixel buffer, and texture cache
//! are fully implemented via ash. Blitter and format converter are partial.

mod device_capability;
mod vulkan_command_buffer;
mod vulkan_command_queue;
mod vulkan_device;
mod vulkan_sync;
mod vulkan_texture;

pub use device_capability::{ConsumerMarker, DevicePrivilege, HostMarker, VulkanRhiDevice};
pub use vulkan_command_buffer::VulkanCommandBuffer;
pub use vulkan_command_queue::VulkanCommandQueue;
pub use vulkan_device::VulkanDevice;
#[allow(unused_imports)]
pub use vulkan_sync::{VulkanFence, VulkanSemaphore};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use vulkan_sync::VulkanTimelineSemaphore;
pub use vulkan_texture::VulkanTexture;

mod vulkan_blitter;
pub use vulkan_blitter::VulkanBlitter;

pub(crate) mod vulkan_pixel_buffer;
pub use vulkan_pixel_buffer::VulkanPixelBuffer;

mod vulkan_texture_cache;
pub use vulkan_texture_cache::VulkanTextureCache;

mod vulkan_pixel_buffer_pool;
pub use vulkan_pixel_buffer_pool::VulkanPixelBufferPool;

mod vulkan_compute_kernel;
pub use vulkan_compute_kernel::VulkanComputeKernel;

mod vulkan_format_converter;
pub use vulkan_format_converter::VulkanFormatConverter;

#[cfg(target_os = "linux")]
pub mod drm_modifier_probe;

#[cfg(all(test, target_os = "linux"))]
mod vulkan_swapchain_alloc_repro_test;
