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

/// Host-side gateway for `vkQueueSubmit` calls.
pub trait RhiQueueSubmitter: Send + Sync {
    /// Submit command buffers using the Vulkan 1.0 submit API under host
    /// synchronization.
    ///
    /// # Safety
    ///
    /// Caller must ensure the command buffers, submit info chains, and fence
    /// are valid Vulkan handles and that `queue` belongs to the host device.
    unsafe fn submit_to_queue_legacy(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo],
        fence: vk::Fence,
    ) -> VkResult<()>;
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
    unsafe fn submit_to_queue_legacy(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo],
        fence: vk::Fence,
    ) -> VkResult<()> {
        self.device.queue_submit(queue, submits, fence).map(|_| ())
    }
}
