// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host RHI integration points for the codec layer.
//!
//! The codec layer submits command buffers through an [`RhiQueueSubmitter`]
//! supplied by the host so the host's per-queue synchronization (mutexes,
//! command queue ownership) is honored. Without this, concurrent submissions
//! from streamlib processors and codec encode/decode threads race on the
//! same `VkQueue` and crash the NVIDIA driver.

use vulkanalia::vk;
use vulkanalia::VkResult;

/// Host-side gateway between the codec layer and the engine RHI's
/// per-queue mutex + device-resource lock. Sole implementor is
/// [`crate::vulkan::rhi::HostVulkanDevice`]; the trait abstraction
/// scopes the codec layer to two operations (serialized `vkQueueSubmit2`
/// and device-level resource-creation lock) instead of letting it reach
/// into the broader host RHI surface.
///
/// Stays `pub` because several codec types (`VulkanVideoSession`,
/// `VkVideoDecoder`, `RgbToNv12Converter`, etc.) hold
/// `Arc<dyn RhiQueueSubmitter>` fields — Rust's privacy rules require
/// the trait to be at least as visible as those items. The trait itself
/// is not part of any consumer-facing API; the codec's public surface
/// is `SimpleEncoder::from_full_access` / `SimpleDecoder::from_full_access`,
/// which wire the submitter internally. Eventual removal of this trait
/// (call `HostVulkanDevice` methods directly) is interior re-plumbing
/// work tracked under the Vulkan Video RHI Coupling milestone.
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

    /// Run `f` while holding the host's device-level resource-creation lock,
    /// the same lock the new RHI primitives
    /// ([`HostVulkanVideoSession`](crate::vulkan::rhi::HostVulkanVideoSession),
    /// [`HostVulkanTexture::new_video_dpb`](crate::vulkan::rhi::HostVulkanTexture),
    /// [`HostVulkanBuffer::new_video_bitstream`](crate::vulkan::rhi::HostVulkanBuffer))
    /// hold internally via `HostVulkanDevice::lock_device`. Remaining callers
    /// of this shim are submit-side staging ops in `encode/staging.rs` —
    /// scope of the milestone capstone that retires the trait entirely.
    fn with_device_resource_lock(&self, f: &mut dyn FnMut());
}
