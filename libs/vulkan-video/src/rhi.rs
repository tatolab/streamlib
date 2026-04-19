// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host RHI integration points for vulkan-video.
//!
//! vulkan-video submits command buffers through an [`RhiQueueSubmitter`]
//! supplied by the host so the host's per-queue synchronization (mutexes,
//! command queue ownership) is honored. Without this, concurrent submissions
//! from streamlib processors and vulkan-video encode/decode threads race on
//! the same `VkQueue` and crash the NVIDIA driver.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::VkResult;

/// Host-side gateway for `vkQueueSubmit2` calls.
pub trait RhiQueueSubmitter: Send + Sync {
    /// Submit command buffers using the Vulkan 1.4 sync2 submit API under
    /// host synchronization.
    ///
    /// # Safety
    ///
    /// Caller must ensure the command buffers, submit info chains, and fence
    /// are valid Vulkan handles and that `queue` belongs to the host device.
    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> VkResult<()>;

    /// Run `f` while holding the host's device-level resource-creation lock.
    /// Wraps `vkCreateVideoSessionKHR`, DPB image allocation, bitstream buffer
    /// allocation, and `vkBindVideoSessionMemoryKHR` so they cannot race with
    /// concurrent submissions from other processors on NVIDIA Linux.
    fn with_device_resource_lock(&self, f: &mut dyn FnMut());
}

/// Unsynchronized submitter used when vulkan-video owns its own Vulkan device
/// (the `SimpleEncoder::new` / `SimpleDecoder::new` paths and standalone
/// binaries). No concurrent submissions happen in those cases, so we submit
/// directly without taking any lock.
pub struct RawQueueSubmitter {
    device: vulkanalia::Device,
}

impl RawQueueSubmitter {
    pub fn new(device: vulkanalia::Device) -> Arc<dyn RhiQueueSubmitter> {
        Arc::new(Self { device })
    }
}

impl RhiQueueSubmitter for RawQueueSubmitter {
    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> VkResult<()> {
        self.device.queue_submit2(queue, submits, fence).map(|_| ())
    }

    fn with_device_resource_lock(&self, f: &mut dyn FnMut()) {
        // Standalone mode owns the Vulkan device exclusively; no concurrent
        // submissions exist, so no locking is required.
        f();
    }
}
