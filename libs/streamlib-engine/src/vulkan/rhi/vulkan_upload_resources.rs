// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pre-allocated `vkCmdCopyBufferToImage` command resources for the
//! hot path.
//!
//! Each instance owns a private command pool + command buffer + fence,
//! constructed once and reused per frame. Pairs with
//! [`HostVulkanDevice::upload_buffer_to_image_amortized`] to eliminate
//! the per-call `vkCreateCommandPool` / `vkAllocateCommandBuffers` /
//! `vkCreateFence` / destroy churn that the per-call
//! [`HostVulkanDevice::upload_buffer_to_image`] path pays.
//!
//! Pool-per-instance (rather than a shared pool across N instances)
//! sidesteps Vulkan's external-synchronization requirement on
//! `vkResetCommandBuffer` — separate threads can reset / record their
//! own slot's cb in parallel without mutexing the pool.

use std::sync::Arc;

use vulkanalia::vk;
use vulkanalia::vk::{DeviceV1_0, Handle, HasBuilder};

use super::vulkan_device::HostVulkanDevice;
use crate::core::{Error, Result};

/// Owns a private `VkCommandPool` + `VkCommandBuffer` + `VkFence` for
/// amortized buffer-to-image uploads. Construction allocates the
/// resources; `Drop` destroys them.
///
/// Not `Clone` — every instance owns unique Vulkan handles.
pub struct HostVulkanUploadResources {
    device: Arc<HostVulkanDevice>,
    pool: vk::CommandPool,
    cb: vk::CommandBuffer,
    fence: vk::Fence,
}

impl HostVulkanUploadResources {
    /// Allocate a private pool + cb + fence on the given device's
    /// graphics queue family. The pool carries
    /// `RESET_COMMAND_BUFFER_BIT` so per-frame
    /// `vkResetCommandBuffer` is legal without affecting other pools.
    pub fn new(device: &Arc<HostVulkanDevice>) -> Result<Self> {
        let queue_family = device.queue_family_index();
        let raw_device = device.device();

        let pool = unsafe {
            raw_device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .map_err(|e| Error::GpuError(format!("HostVulkanUploadResources pool: {e}")))?;

        let cb = unsafe {
            raw_device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| Error::GpuError(format!("HostVulkanUploadResources cb: {e}")))?[0];

        // Create the fence SIGNALED so:
        //   - first amortized submit's reset_fences is legal (reset is
        //     valid on signaled fences) and waits correctly afterward,
        //   - Drop on a never-used ring returns immediately from
        //     wait_for_fences (the fence is already in the signaled
        //     state, so the wait is a no-op).
        // This is the canonical Vulkan frames-in-flight pattern.
        let fence = unsafe {
            raw_device.create_fence(
                &vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )
        }
        .map_err(|e| Error::GpuError(format!("HostVulkanUploadResources fence: {e}")))?;

        tracing::trace!(
            target: "streamlib::vulkan::upload_resources",
            pool = pool.as_raw(),
            cb = cb.as_raw(),
            fence = fence.as_raw(),
            "HostVulkanUploadResources allocated"
        );

        Ok(Self {
            device: Arc::clone(device),
            pool,
            cb,
            fence,
        })
    }

    /// Borrow the command buffer for amortized upload.
    pub fn command_buffer(&self) -> vk::CommandBuffer {
        self.cb
    }

    /// Borrow the fence the amortized upload submits with.
    pub fn fence(&self) -> vk::Fence {
        self.fence
    }
}

impl Drop for HostVulkanUploadResources {
    fn drop(&mut self) {
        let raw_device = self.device.device();
        // wait_for_fences in case a submit is mid-flight — safety
        // belt-and-braces. The fence is reset before each submit, so
        // if it's signaled it means the last submit completed; if
        // unsignaled and no submit pending, the wait returns
        // immediately (timeout 0 would be a tiny micro-opt but
        // u64::MAX matches the upload-path wait).
        let _ = unsafe { raw_device.wait_for_fences(&[self.fence], true, u64::MAX) };
        unsafe { raw_device.destroy_fence(self.fence, None) };
        // Destroying the pool frees the cb automatically.
        unsafe { raw_device.destroy_command_pool(self.pool, None) };
        tracing::trace!(
            target: "streamlib::vulkan::upload_resources",
            "HostVulkanUploadResources dropped"
        );
    }
}
