// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Engine-owned multi-step command-buffer recorder.

use std::ffi::c_void;
use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{Texture, VulkanLayout};
use crate::core::{Error, Result};

use crate::vulkan::rhi::{
    HostVulkanDevice, HostVulkanQueueSubmissionsSnapshot, HostVulkanTimelineSemaphore,
    VulkanAccess, VulkanBufferLike, VulkanComputeKernel, VulkanStage,
};

mod command_recorder_barriers_and_layout_transitions;
mod command_recorder_copies;
mod command_recorder_dispatch_and_submission;
mod rhi_command_recorder_handle;
#[cfg(test)]
mod tests;

pub use rhi_command_recorder_handle::RhiCommandRecorder;

/// Image-to-buffer / buffer-to-image copy region.
///
/// Wraps the most common shape of `VkBufferImageCopy` — single mip
/// level, single array layer, color aspect, full image — without
/// dragging callers through `vulkanalia` imports. For mip-mapped or
/// multi-layer copies, file a follow-up; today's in-tree call sites
/// (camera readback, cpu-readback adapter, texture readback) all copy
/// a single mip / single layer / color aspect.
#[derive(Clone, Copy, Debug)]
pub struct ImageCopyRegion {
    pub width: u32,
    pub height: u32,
    pub buffer_offset: u64,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub mip_level: u32,
    pub array_layer: u32,
}

impl ImageCopyRegion {
    /// Tightly-packed region: buffer rows match image width, no offset,
    /// mip 0 / layer 0 / color aspect.
    pub fn tightly_packed(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            buffer_offset: 0,
            buffer_row_length: width,
            buffer_image_height: height,
            mip_level: 0,
            array_layer: 0,
        }
    }

    fn to_vk(self) -> vk::BufferImageCopy {
        vk::BufferImageCopy::builder()
            .buffer_offset(self.buffer_offset)
            .buffer_row_length(self.buffer_row_length)
            .buffer_image_height(self.buffer_image_height)
            .image_subresource(
                vk::ImageSubresourceLayers::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(self.mip_level)
                    .base_array_layer(self.array_layer)
                    .layer_count(1)
                    .build(),
            )
            .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .image_extent(vk::Extent3D {
                width: self.width,
                height: self.height,
                depth: 1,
            })
            .build()
    }
}

/// The layout an image comes back to after a transfer: the one it was in,
/// or GENERAL for an image nothing had written, since UNDEFINED is never a
/// barrier target.
fn layout_an_image_rests_in_after_a_transfer(known_layout: VulkanLayout) -> VulkanLayout {
    if known_layout == VulkanLayout::UNDEFINED {
        VulkanLayout::GENERAL
    } else {
        known_layout
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecorderState {
    /// No active recording. `begin()` is permitted; `record_*` and
    /// `submit_*` are typed errors.
    Idle,
    /// A recording is in progress. `record_*` and `submit_*` are
    /// permitted; `begin()` is a typed error.
    Recording,
}

/// Engine-owned multi-step command-buffer recorder.
///
/// Owns a long-lived command pool + reset-able primary command buffer
/// for serialized recording, plus an internal completion fence that
/// `Drop` waits on so the command buffer is never freed mid-flight.
///
/// Per-frame use:
/// 1. `begin()` — waits for the prior submission, resets the buffer.
/// 2. `record_image_barrier` / `record_buffer_barrier` /
///    `record_copy_*` / `record_dispatch` — append work.
/// 3. `submit_signaling_timeline(...)` or `submit_and_wait()` —
///    closes the recording, submits, optionally signals an
///    external timeline semaphore at a target value.
///
/// Recording is serial (one recording in flight at a time per
/// recorder handle). For parallel recording, hold one recorder per
/// in-flight slot.
/// Rich data backing a [`RhiCommandRecorder`], reached through the
/// recorder's opaque handle.
pub struct RhiCommandRecorderInner {
    label: String,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    /// Signaled on `vkQueueSubmit2` completion and waited on at the
    /// next `begin()`. Tracked together with `submission_in_flight` so
    /// a failed submit (fence stays unsignaled) doesn't deadlock the
    /// next `begin()`'s wait.
    completion_fence: vk::Fence,
    /// `true` between a successful `submit_to_queue` and the next
    /// `begin()`'s wait/reset. Initialized `false` (no prior submit).
    /// A submit call sets this only AFTER `vkQueueSubmit2` returns
    /// success — if any earlier step (or the submit itself) fails,
    /// `submission_in_flight` stays `false` and the next `begin()`
    /// skips the fence wait. This is the defensive path for the
    /// fail-then-deadlock failure mode that the bare fence pattern
    /// (`VulkanComputeKernel` / `VulkanGraphicsKernel`) inherits from
    /// raw Vulkan — `DEVICE_LOST` / `OUT_OF_DEVICE_MEMORY` on
    /// `vkQueueSubmit` leaves the fence unsignaled forever.
    submission_in_flight: bool,
    /// The queue completion timeline value the latest accepted submission
    /// signals.
    latest_submission: HostVulkanQueueSubmissionsSnapshot,
    state: Mutex<RecorderState>,
}

impl RhiCommandRecorderInner {
    /// Build a recorder against the device's default queue.
    ///
    /// `label` flows into `tracing` spans on every public method —
    /// pick something processor-scoped (e.g. `"camera"`, `"display"`).
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(label))]
    pub(crate) fn new(vulkan_device: &Arc<HostVulkanDevice>, label: &str) -> Result<Self> {
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();
        let device = vulkan_device.device().clone();

        let pool_info = vk::CommandPoolCreateInfo::builder()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family_index)
            .build();
        let command_pool =
            unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
                Error::GpuError(format!(
                    "RhiCommandRecorder '{label}': vkCreateCommandPool failed: {e}"
                ))
            })?;

        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let buffers = match unsafe { device.allocate_command_buffers(&alloc_info) } {
            Ok(b) => b,
            Err(e) => {
                unsafe { device.destroy_command_pool(command_pool, None) };
                return Err(Error::GpuError(format!(
                    "RhiCommandRecorder '{label}': vkAllocateCommandBuffers failed: {e}"
                )));
            }
        };
        let command_buffer = buffers[0];

        // Starts unsignaled — first `begin()` sees
        // `submission_in_flight = false` and skips the fence wait.
        let fence_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::empty())
            .build();
        let completion_fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                }
                return Err(Error::GpuError(format!(
                    "RhiCommandRecorder '{label}': vkCreateFence failed: {e}"
                )));
            }
        };

        Ok(Self {
            label: label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            device,
            queue,
            command_pool,
            command_buffer,
            completion_fence,
            submission_in_flight: false,
            latest_submission: HostVulkanQueueSubmissionsSnapshot::default(),
            state: Mutex::new(RecorderState::Idle),
        })
    }

    /// Begin a new recording.
    ///
    /// Waits on the recorder's completion fence (so the prior
    /// submission has drained and the command buffer is safe to
    /// reset), resets the fence, resets the command buffer, and
    /// begins it with `ONE_TIME_SUBMIT`. After this returns, the
    /// recorder accepts `record_*` calls; the next `begin()` is a
    /// typed error until a `submit_*` closes the recording.
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub fn begin(&mut self) -> Result<()> {
        let mut state = self.state.lock();
        if *state == RecorderState::Recording {
            return Err(Error::GpuError(format!(
                "RhiCommandRecorder '{}': begin() called while a recording is already in progress",
                self.label
            )));
        }

        unsafe {
            if self.submission_in_flight {
                // Once the submission's completion timeline value is reached its
                // work has retired, and the fence signal that may trail it is no
                // stall on GPU work, so that wait goes uncounted.
                let already_signaled = self
                    .device
                    .get_fence_status(self.completion_fence)
                    .is_ok_and(|status| status == vk::SuccessCode::SUCCESS);
                if !already_signaled {
                    let completion_timeline_reached = self
                        .vulkan_device
                        .every_queue_submission_in_snapshot_has_completed(&self.latest_submission)
                        .unwrap_or(false);
                    if completion_timeline_reached {
                        self.device
                            .wait_for_fences(&[self.completion_fence], true, u64::MAX)
                            .map_err(|e| {
                                Error::GpuError(format!(
                                    "RhiCommandRecorder '{}': wait_for_fences at begin(): {e}",
                                    self.label
                                ))
                            })?;
                    } else {
                        self.vulkan_device.wait_for_fences_blocking_counted(
                            &[self.completion_fence],
                            format_args!("RhiCommandRecorder '{}' at begin()", self.label),
                        )?;
                    }
                }
                self.device
                    .reset_fences(&[self.completion_fence])
                    .map_err(|e| {
                        Error::GpuError(format!(
                            "RhiCommandRecorder '{}': reset_fences at begin(): {e}",
                            self.label
                        ))
                    })?;
                self.submission_in_flight = false;
            }
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': reset_command_buffer at begin(): {e}",
                        self.label
                    ))
                })?;
            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': begin_command_buffer: {e}",
                        self.label
                    ))
                })?;
        }

        *state = RecorderState::Recording;
        Ok(())
    }

    /// Abandon an in-progress recording without submitting: reset the
    /// state machine to [`RecorderState::Idle`] so the next [`Self::begin`]
    /// starts clean. The begun-but-unsubmitted command buffer is discarded at
    /// that `begin()`'s `reset_command_buffer`. Used when a failed record
    /// abandons a recording, so the recorder is reusable next attempt.
    /// Harmless when nothing is recording.
    pub fn abort_recording(&mut self) {
        *self.state.lock() = RecorderState::Idle;
    }

    fn expect_recording(&self, op: &'static str) -> Result<()> {
        let state = self.state.lock();
        if *state != RecorderState::Recording {
            return Err(Error::GpuError(format!(
                "RhiCommandRecorder '{}': {op} called outside an active recording — call begin() first",
                self.label
            )));
        }
        Ok(())
    }
}

impl Drop for RhiCommandRecorderInner {
    fn drop(&mut self) {
        unsafe {
            // Wait only for THIS recorder's own GPU work to drain before
            // freeing its command-buffer / fence. `device_wait_idle()`
            // would serialize the entire device — a classic Vulkan
            // anti-pattern that wedges hot paths whose recorders drop
            // at frame rate (the tone-mapper's `apply_with_layouts`
            // creates a fresh recorder per dispatch). Wait the local
            // fence iff a submission is actually in flight; otherwise
            // the recorder either never submitted (no fence to wait on)
            // or has already waited at `begin()` / `submit_and_wait()`
            // time, and the fence is signaled.
            if self.submission_in_flight {
                let _ = self
                    .device
                    .wait_for_fences(&[self.completion_fence], true, u64::MAX);
            }
            self.device.destroy_fence(self.completion_fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

// The recorder holds Vulkan handles that are guarded by its internal
// fence + state machine; the `Mutex<RecorderState>` serializes state
// transitions across threads. `&mut self` on every public method
// further enforces single-thread use.
unsafe impl Send for RhiCommandRecorderInner {}
unsafe impl Sync for RhiCommandRecorderInner {}

impl std::fmt::Debug for RhiCommandRecorderInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiCommandRecorderInner")
            .field("label", &self.label)
            .field("state", &*self.state.lock())
            .finish()
    }
}
#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;

    #[test]
    fn rhi_command_recorder_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RhiCommandRecorder>();
    }

    /// `RhiCommandRecorder` is intentionally NOT `Clone` — recording
    /// state doesn't survive duplication. Marker test for `cargo test`
    /// discoverability; the type-level absence-of-Clone enforces it.
    #[test]
    fn rhi_command_recorder_is_not_clone_marker() {
        // No-op; the type has no Clone impl.
    }
}
