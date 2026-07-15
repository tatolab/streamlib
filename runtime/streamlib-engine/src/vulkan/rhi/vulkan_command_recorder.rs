// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-owned multi-step command-buffer recorder.

use std::ffi::c_void;
use std::sync::Arc;

use parking_lot::Mutex;
use streamlib_plugin_abi::GpuContextFullAccessVTable;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{DrawCall, DrawIndexedCall, ScissorRect, Texture, Viewport, VulkanLayout};
use crate::core::{Error, Result};

use super::{
    HostVulkanDevice, HostVulkanTimelineSemaphore, VulkanAccess, VulkanBufferLike,
    VulkanComputeKernel, VulkanGraphicsKernel, VulkanStage,
};

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
/// Host-only rich data backing a [`RhiCommandRecorder`]. Cdylib code
/// never sees this type; it reaches the public surface through the
/// `(handle, vtable)` PluginAbiObject.
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
                self.device
                    .wait_for_fences(&[self.completion_fence], true, u64::MAX)
                    .map_err(|e| {
                        Error::GpuError(format!(
                            "RhiCommandRecorder '{}': wait_for_fences at begin(): {e}",
                            self.label
                        ))
                    })?;
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

    /// Image layout transition. Caller supplies `from_layout` (typically
    /// `registration.current_layout()` from `TextureRegistration`), the
    /// target `to_layout`, and the surrounding stage/access masks.
    /// Records a single `cmd_pipeline_barrier2` with one image memory
    /// barrier; updating any `TextureRegistration` after the barrier is
    /// the caller's responsibility.
    #[tracing::instrument(level = "trace", skip(self, texture), fields(label = %self.label, from = ?from_layout, to = ?to_layout))]
    pub fn record_image_barrier(
        &mut self,
        texture: &Texture,
        from_layout: VulkanLayout,
        to_layout: VulkanLayout,
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        self.expect_recording("record_image_barrier")?;

        use crate::host_rhi::HostTextureExt;
        let image = texture.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_image_barrier: texture has no VkImage",
                self.label
            ))
        })?;

        let subresource = vk::ImageSubresourceRange::builder()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .base_mip_level(0)
            .level_count(1)
            .base_array_layer(0)
            .layer_count(1)
            .build();

        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(from_stage.as_vk())
            .src_access_mask(from_access.as_vk())
            .dst_stage_mask(to_stage.as_vk())
            .dst_access_mask(to_access.as_vk())
            .old_layout(from_layout.as_vk())
            .new_layout(to_layout.as_vk())
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(subresource)
            .build();

        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers)
            .build();
        unsafe {
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);
        }
        Ok(())
    }

    /// Buffer memory barrier covering the whole buffer.
    #[tracing::instrument(level = "trace", skip(self, buffer), fields(label = %self.label))]
    pub fn record_buffer_barrier(
        &mut self,
        buffer: &(impl VulkanBufferLike + ?Sized),
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        self.expect_recording("record_buffer_barrier")?;

        let barrier = vk::BufferMemoryBarrier2::builder()
            .src_stage_mask(from_stage.as_vk())
            .src_access_mask(from_access.as_vk())
            .dst_stage_mask(to_stage.as_vk())
            .dst_access_mask(to_access.as_vk())
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(buffer.vk_buffer())
            .offset(0)
            .size(buffer.vk_buffer_size())
            .build();

        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .buffer_memory_barriers(&barriers)
            .build();
        unsafe {
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);
        }
        Ok(())
    }

    /// Record `vkCmdCopyImageToBuffer`. The source texture must already
    /// be in `src_layout` (typically `TRANSFER_SRC_OPTIMAL`); transition
    /// it there via [`Self::record_image_barrier`] first.
    #[tracing::instrument(level = "trace", skip(self, src, dst, region), fields(label = %self.label))]
    pub fn record_copy_image_to_buffer(
        &mut self,
        src: &Texture,
        src_layout: VulkanLayout,
        dst: &(impl VulkanBufferLike + ?Sized),
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.expect_recording("record_copy_image_to_buffer")?;

        use crate::host_rhi::HostTextureExt;
        let image = src.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_image_to_buffer: source texture has no VkImage",
                self.label
            ))
        })?;

        let region_vk = region.to_vk();
        unsafe {
            self.device.cmd_copy_image_to_buffer(
                self.command_buffer,
                image,
                src_layout.as_vk(),
                dst.vk_buffer(),
                &[region_vk],
            );
        }
        Ok(())
    }

    /// Record `vkCmdCopyBufferToImage`. The destination texture must
    /// already be in `dst_layout` (typically `TRANSFER_DST_OPTIMAL`);
    /// transition it there via [`Self::record_image_barrier`] first.
    #[tracing::instrument(level = "trace", skip(self, src, dst, region), fields(label = %self.label))]
    pub fn record_copy_buffer_to_image(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &Texture,
        dst_layout: VulkanLayout,
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.expect_recording("record_copy_buffer_to_image")?;

        use crate::host_rhi::HostTextureExt;
        let image = dst.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_buffer_to_image: dest texture has no VkImage",
                self.label
            ))
        })?;

        let region_vk = region.to_vk();
        unsafe {
            self.device.cmd_copy_buffer_to_image(
                self.command_buffer,
                src.vk_buffer(),
                image,
                dst_layout.as_vk(),
                &[region_vk],
            );
        }
        Ok(())
    }

    /// Record a compute dispatch via [`VulkanComputeKernel::record`]
    /// into the recorder's command buffer.
    ///
    /// Bindings + push constants must have been staged on `kernel` via
    /// its `set_*` methods before this call. The kernel's descriptor
    /// set is shared across calls; per the kernel's contract no
    /// concurrent dispatch/record against the same kernel may be in
    /// flight.
    #[tracing::instrument(level = "trace", skip(self, kernel), fields(label = %self.label, group_x, group_y, group_z))]
    pub fn record_dispatch(
        &mut self,
        kernel: &VulkanComputeKernel,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        self.expect_recording("record_dispatch")?;
        kernel.record(self.command_buffer, group_x, group_y, group_z)
    }

    /// Record a draw via [`VulkanGraphicsKernel::cmd_bind_and_draw`]
    /// into the recorder's command buffer.
    ///
    /// Must be called inside an active render pass (e.g. between
    /// [`PresentFrame::begin_rendering`](super::vulkan_present_target::PresentFrame::begin_rendering)
    /// and `end_rendering`). Bindings + push constants for `frame_index`
    /// must have been staged via the kernel's `set_*` methods before this
    /// call; the kernel drains them on entry.
    #[tracing::instrument(level = "trace", skip(self, kernel, draw), fields(label = %self.label, frame_index))]
    pub fn record_draw(
        &mut self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawCall,
    ) -> Result<()> {
        self.expect_recording("record_draw")?;
        kernel.cmd_bind_and_draw(self.command_buffer, frame_index, draw)
    }

    /// Indexed-draw variant of [`Self::record_draw`]. Caller must have
    /// set an index buffer for `frame_index` via
    /// [`VulkanGraphicsKernel::set_index_buffer`].
    #[tracing::instrument(level = "trace", skip(self, kernel, draw), fields(label = %self.label, frame_index))]
    pub fn record_draw_indexed(
        &mut self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawIndexedCall,
    ) -> Result<()> {
        self.expect_recording("record_draw_indexed")?;
        kernel.cmd_bind_and_draw_indexed(self.command_buffer, frame_index, draw)
    }

    /// Engine-internal submit path supporting binary + timeline waits
    /// and signals, used by [`VulkanPresentTarget`](super::vulkan_present_target::VulkanPresentTarget)
    /// for the swapchain image-available wait → render-finished binary
    /// signal + frame-timeline signal dance that `submit_signaling_timeline`
    /// can't express. Mirrors [`Self::submit_inner`]'s recorder-state and
    /// fence bookkeeping so the next [`Self::begin`] waits correctly.
    pub(crate) fn submit_with_semaphores(
        &mut self,
        waits: &[vk::SemaphoreSubmitInfo],
        signals: &[vk::SemaphoreSubmitInfo],
    ) -> Result<()> {
        {
            let mut state = self.state.lock();
            if *state != RecorderState::Recording {
                return Err(Error::GpuError(format!(
                    "RhiCommandRecorder '{}': submit_with_semaphores called without an active recording",
                    self.label
                )));
            }
            *state = RecorderState::Idle;
        }

        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': end_command_buffer: {e}",
                        self.label
                    ))
                })?;
        }

        let cmd_info = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(self.command_buffer)
            .build();
        let cmd_infos = [cmd_info];

        let submit = vk::SubmitInfo2::builder()
            .wait_semaphore_infos(waits)
            .command_buffer_infos(&cmd_infos)
            .signal_semaphore_infos(signals)
            .build();

        unsafe {
            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], self.completion_fence)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': submit_to_queue: {e}",
                        self.label
                    ))
                })?;
        }

        self.submission_in_flight = true;

        Ok(())
    }

    /// End recording and submit, signaling `timeline` at `signal_value`
    /// on completion. The recorder's internal completion fence is also
    /// signaled so the next `begin()` waits the right amount.
    ///
    /// `signal_value` MUST be strictly greater than the timeline's
    /// current counter — Vulkan disallows monotonic regressions.
    #[tracing::instrument(level = "trace", skip(self, timeline), fields(label = %self.label, signal_value))]
    pub fn submit_signaling_timeline(
        &mut self,
        timeline: &HostVulkanTimelineSemaphore,
        signal_value: u64,
    ) -> Result<()> {
        self.submit_inner(Some((timeline.semaphore(), signal_value)))
    }

    /// End recording and submit without semaphore signaling. The
    /// recorder's internal completion fence is signaled so the next
    /// `begin()` blocks on completion.
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub fn submit(&mut self) -> Result<()> {
        self.submit_inner(None)
    }

    /// End recording, submit, and block until the GPU completes.
    /// Convenience for one-shot setup paths and tests; per-frame paths
    /// should prefer [`Self::submit_signaling_timeline`] and have the
    /// caller wait on the timeline at frame boundaries.
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub fn submit_and_wait(&mut self) -> Result<()> {
        self.submit_inner(None)?;
        unsafe {
            self.device
                .wait_for_fences(&[self.completion_fence], true, u64::MAX)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': wait_for_fences in submit_and_wait: {e}",
                        self.label
                    ))
                })?;
        }
        Ok(())
    }

    fn submit_inner(&mut self, timeline_signal: Option<(vk::Semaphore, u64)>) -> Result<()> {
        {
            let mut state = self.state.lock();
            if *state != RecorderState::Recording {
                return Err(Error::GpuError(format!(
                    "RhiCommandRecorder '{}': submit called without an active recording",
                    self.label
                )));
            }
            *state = RecorderState::Idle;
        }

        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': end_command_buffer: {e}",
                        self.label
                    ))
                })?;
        }

        let cmd_info = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(self.command_buffer)
            .build();
        let cmd_infos = [cmd_info];

        let signal_infos;
        let submit = match timeline_signal {
            Some((semaphore, value)) => {
                let info = vk::SemaphoreSubmitInfo::builder()
                    .semaphore(semaphore)
                    .value(value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build();
                signal_infos = [info];
                vk::SubmitInfo2::builder()
                    .command_buffer_infos(&cmd_infos)
                    .signal_semaphore_infos(&signal_infos)
                    .build()
            }
            None => vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build(),
        };

        unsafe {
            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], self.completion_fence)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': submit_to_queue: {e}",
                        self.label
                    ))
                })?;
        }

        // Only mark in-flight AFTER the queue accepted the submission.
        // If submit failed (or any earlier step did), the flag stays
        // `false` and the next `begin()` skips the fence wait — which
        // would otherwise deadlock on an unsignaled fence.
        self.submission_in_flight = true;

        Ok(())
    }

    /// Record a layout transition on a raw `VkImage` handle.
    /// Distinct from [`Self::record_image_barrier`] which takes a
    /// `Texture` PluginAbiObject; this variant is used by
    /// [`VulkanPresentTarget`](super::vulkan_present_target::VulkanPresentTarget)
    /// for swapchain images (which are never wrapped in a `Texture`).
    /// COLOR aspect, single mip / single layer, QUEUE_FAMILY_IGNORED
    /// on both sides.
    pub(crate) fn record_swapchain_image_barrier(
        &mut self,
        image: vk::Image,
        old_layout: vk::ImageLayout,
        new_layout: vk::ImageLayout,
        src_stage: vk::PipelineStageFlags2,
        src_access: vk::AccessFlags2,
        dst_stage: vk::PipelineStageFlags2,
        dst_access: vk::AccessFlags2,
    ) -> Result<()> {
        self.expect_recording("record_swapchain_image_barrier")?;
        let subresource = vk::ImageSubresourceRange::builder()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .base_mip_level(0)
            .level_count(1)
            .base_array_layer(0)
            .layer_count(1)
            .build();
        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(src_stage)
            .src_access_mask(src_access)
            .dst_stage_mask(dst_stage)
            .dst_access_mask(dst_access)
            .old_layout(old_layout)
            .new_layout(new_layout)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(subresource)
            .build();
        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers)
            .build();
        unsafe {
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);
        }
        Ok(())
    }

    /// Begin a dynamic-rendering pass against a caller-owned
    /// `VkImageView`. CLEAR load op when `clear_color` is `Some`,
    /// LOAD otherwise. The image must already be in
    /// `COLOR_ATTACHMENT_OPTIMAL` (transition via
    /// [`Self::record_swapchain_image_barrier`] beforehand). Used
    /// by [`PresentFrame::begin_rendering`](super::vulkan_present_target::PresentFrame::begin_rendering).
    pub(crate) fn cmd_begin_dynamic_rendering(
        &mut self,
        image_view: vk::ImageView,
        extent: (u32, u32),
        clear_color: Option<[f32; 4]>,
    ) -> Result<()> {
        self.expect_recording("cmd_begin_dynamic_rendering")?;
        let load_op = if clear_color.is_some() {
            vk::AttachmentLoadOp::CLEAR
        } else {
            vk::AttachmentLoadOp::LOAD
        };
        let clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: clear_color.unwrap_or([0.0, 0.0, 0.0, 1.0]),
            },
        };
        let color_attachment = vk::RenderingAttachmentInfo::builder()
            .image_view(image_view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(load_op)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(clear_value)
            .build();
        let color_attachments = [color_attachment];
        let rendering_info = vk::RenderingInfo::builder()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: extent.0,
                    height: extent.1,
                },
            })
            .layer_count(1)
            .color_attachments(&color_attachments)
            .build();
        unsafe {
            self.device
                .cmd_begin_rendering(self.command_buffer, &rendering_info);
        }
        Ok(())
    }

    /// Close the dynamic-rendering pass opened by
    /// [`Self::cmd_begin_dynamic_rendering`].
    pub(crate) fn cmd_end_dynamic_rendering(&mut self) -> Result<()> {
        self.expect_recording("cmd_end_dynamic_rendering")?;
        unsafe {
            self.device.cmd_end_rendering(self.command_buffer);
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // From-wire shims used by host extern "C" callbacks in
    // `core/plugin/host_services.rs`. The check-boundaries rule keeps
    // raw `vulkanalia` imports inside the RHI / consumer-rhi / adapter
    // crates; the host wrappers receive raw integer wire types and
    // dispatch through these shims so all `vk::*` construction happens
    // inside `vulkan/rhi/`.
    // -------------------------------------------------------------------------

    /// Wire-format companion to
    /// [`Self::record_swapchain_image_barrier`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_swapchain_image_barrier_from_wire(
        &mut self,
        image_raw: u64,
        from_layout_raw: i32,
        to_layout_raw: i32,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
    ) -> Result<()> {
        let image = vk::Image::from_raw(image_raw);
        let old_layout = vk::ImageLayout::from_raw(from_layout_raw);
        let new_layout = vk::ImageLayout::from_raw(to_layout_raw);
        let src_stage = vk::PipelineStageFlags2::from_bits_truncate(from_stage_raw as u64);
        let src_access = vk::AccessFlags2::from_bits_truncate(from_access_raw as u64);
        let dst_stage = vk::PipelineStageFlags2::from_bits_truncate(to_stage_raw as u64);
        let dst_access = vk::AccessFlags2::from_bits_truncate(to_access_raw as u64);
        self.record_swapchain_image_barrier(
            image, old_layout, new_layout, src_stage, src_access, dst_stage, dst_access,
        )
    }

    /// Wire-format companion to [`Self::cmd_begin_dynamic_rendering`].
    pub(crate) fn cmd_begin_dynamic_rendering_from_wire(
        &mut self,
        image_view_raw: u64,
        extent_w: u32,
        extent_h: u32,
        clear_color: Option<[f32; 4]>,
    ) -> Result<()> {
        let image_view = vk::ImageView::from_raw(image_view_raw);
        self.cmd_begin_dynamic_rendering(image_view, (extent_w, extent_h), clear_color)
    }

    /// Wire-format companion to [`Self::submit_with_semaphores`].
    pub(crate) fn submit_with_semaphores_from_wire(
        &mut self,
        waits_repr: &[streamlib_plugin_abi::SemaphoreSubmitInfoRepr],
        signals_repr: &[streamlib_plugin_abi::SemaphoreSubmitInfoRepr],
    ) -> Result<()> {
        let waits: Vec<vk::SemaphoreSubmitInfo> = waits_repr
            .iter()
            .map(|r| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(vk::Semaphore::from_raw(r.semaphore))
                    .value(r.value)
                    .stage_mask(vk::PipelineStageFlags2::from_bits_truncate(r.stage_mask))
                    .device_index(r.device_index)
                    .build()
            })
            .collect();
        let signals: Vec<vk::SemaphoreSubmitInfo> = signals_repr
            .iter()
            .map(|r| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(vk::Semaphore::from_raw(r.semaphore))
                    .value(r.value)
                    .stage_mask(vk::PipelineStageFlags2::from_bits_truncate(r.stage_mask))
                    .device_index(r.device_index)
                    .build()
            })
            .collect();
        self.submit_with_semaphores(&waits, &signals)
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

// =============================================================================
// PluginAbiObject implementation
// =============================================================================

/// Multi-step command-buffer recorder.
///
/// Layout-stable `#[repr(C)] (handle, vtable)` PluginAbiObject. The opaque
/// handle points at a `Box<RhiCommandRecorderInner>`; lifecycle
/// dispatches through the host-installed FullAccess vtable's
/// `drop_command_recorder` callback (Box::from_raw + drop host-side).
///
/// **Single-owner; deliberately NOT `Clone`.** Recording carries
/// mutable state (`begin()` → `record_*(&mut self)` → `submit_*(&mut
/// self)`) that doesn't survive duplication. The
/// `clone_command_recorder` vtable slot is reserved but never invoked
/// — calling `.clone()` on the public PluginAbiObject is a compile error,
/// locked by the `compile_fail` doctest below:
///
/// ```compile_fail
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<streamlib_engine::vulkan::rhi::RhiCommandRecorder>();
/// ```
///
/// Method dispatch routes through three different vtables depending
/// on the method and call site:
///
/// - Drop runs through [`GpuContextFullAccessVTable::drop_command_recorder`]
///   (the parent vtable).
/// - The six camera-hot-path methods (`begin`, `record_image_barrier`,
///   `record_buffer_barrier`, `record_dispatch`,
///   `record_copy_image_to_buffer`, `submit_signaling_timeline`,
///   `record_swapchain_image_barrier`,
///   `cmd_begin_dynamic_rendering`, `cmd_end_dynamic_rendering`,
///   `submit_with_semaphores`, `record_draw`, `record_draw_indexed`)
///   route through the per-type
///   [`streamlib_plugin_abi::RhiCommandRecorderMethodsVTable`] when
///   called from cdylib code.
/// - The remaining host-only methods (`record_copy_buffer_to_image`,
///   `submit`, `submit_and_wait`) keep their cdylib-mode panic via
///   [`Self::host_inner_mut`]; a follow-up slice lifts each as a
///   consumer arrives.
#[repr(C)]
pub struct RhiCommandRecorder {
    /// Opaque handle to the host's `Box<RhiCommandRecorderInner>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin ABI Drop dispatch.
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch (Phase E
    /// sub-lift slice B — #984). Null in host mode; populated by
    /// [`Self::from_inner`] via
    /// [`crate::core::plugin::host_services::host_rhi_command_recorder_methods_vtable`].
    pub(crate) methods_vtable: *const streamlib_plugin_abi::RhiCommandRecorderMethodsVTable,
}

// SAFETY: handle points at a `Box<RhiCommandRecorderInner>`; Inner
// is Send+Sync (Mutex-guarded state, &mut self method dispatch
// further restricts mutation to one thread at a time).
unsafe impl Send for RhiCommandRecorder {}
unsafe impl Sync for RhiCommandRecorder {}

impl RhiCommandRecorder {
    /// Build a recorder against the device's default queue.
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>, label: &str) -> Result<Self> {
        let inner = RhiCommandRecorderInner::new(vulkan_device, label)?;
        Ok(Self::from_inner(inner))
    }

    /// Internal helper: leak a `Box<RhiCommandRecorderInner>` as the
    /// opaque handle and resolve the host-mode FullAccess + per-type
    /// methods vtables.
    pub(crate) fn from_inner(inner: RhiCommandRecorderInner) -> Self {
        let handle = Box::into_raw(Box::new(inner)) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
        let methods_vtable =
            crate::core::plugin::host_services::host_rhi_command_recorder_methods_vtable();
        Self {
            handle,
            vtable,
            methods_vtable,
        }
    }

    /// Raw `Box<RhiCommandRecorderInner>` handle backing this recorder.
    /// Borrowed, NON-OWNING — used by [`VulkanPresentTarget`] to hand its
    /// internal per-frame recorder back across the plugin ABI `begin_frame`
    /// return + `end_frame` identity check. The caller must never release
    /// it (the present target owns the recorder).
    pub(crate) fn raw_handle(&self) -> *const c_void {
        self.handle
    }

    /// Engine-internal mutable borrow of the host-owned
    /// `RhiCommandRecorderInner`. **Panics if called from cdylib code.**
    pub(crate) fn host_inner_mut(&mut self) -> &mut RhiCommandRecorderInner {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "RhiCommandRecorder::host_inner_mut() reached from cdylib code; \
                 this method must dispatch through the GpuContextFullAccessVTable."
            );
        }
        // SAFETY: `self.handle` is `Box::into_raw(Box<RhiCommandRecorderInner>)`
        // and `&mut self` guarantees no other reference exists.
        unsafe { &mut *(self.handle as *mut RhiCommandRecorderInner) }
    }

    // -------------------------------------------------------------------------
    // Method mirrors. The six camera-hot-path methods (`begin`,
    // `record_image_barrier`, `record_buffer_barrier`,
    // `record_dispatch`, `record_copy_image_to_buffer`,
    // `submit_signaling_timeline`) route through the per-type
    // methods vtable when called from cdylib code (Phase E sub-lift
    // slice B — #984). The remaining methods route via host_inner_mut()
    // with cdylib panic-guard until a future slice lifts them as
    // consumers arrive.
    // -------------------------------------------------------------------------

    /// Begin a new recording. See [`RhiCommandRecorderInner::begin`].
    ///
    /// Mode-routed: in-process callers dispatch through
    /// `host_inner_mut`; cdylib callers dispatch through the per-type
    /// methods vtable (Phase E sub-lift slice B).
    pub fn begin(&mut self) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_begin_via_vtable();
        }
        self.host_inner_mut().begin()
    }

    /// Record an image layout transition. See
    /// [`RhiCommandRecorderInner::record_image_barrier`].
    ///
    /// Mode-routed; see [`Self::begin`] for the dispatch contract.
    pub fn record_image_barrier(
        &mut self,
        texture: &Texture,
        from_layout: VulkanLayout,
        to_layout: VulkanLayout,
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_record_image_barrier_via_vtable(
                texture,
                from_layout,
                to_layout,
                from_stage,
                to_stage,
                from_access,
                to_access,
            );
        }
        self.host_inner_mut().record_image_barrier(
            texture,
            from_layout,
            to_layout,
            from_stage,
            to_stage,
            from_access,
            to_access,
        )
    }

    /// Buffer memory barrier covering the whole buffer.
    ///
    /// Mode-routed; see [`Self::begin`] for the dispatch contract.
    /// **Cdylib path supports
    /// [`crate::core::rhi::StorageBuffer`]- and
    /// [`crate::core::rhi::PixelBuffer`]-flavored buffers today** —
    /// the buffer must report a non-`None`
    /// [`VulkanBufferLike::cdylib_storage_buffer_handle`] or
    /// [`VulkanBufferLike::cdylib_pixel_buffer_handle`] or the
    /// dispatch returns a typed error. Future buffer flavors add
    /// further sibling vtable slots; the host path is unchanged.
    pub fn record_buffer_barrier(
        &mut self,
        buffer: &(impl VulkanBufferLike + ?Sized),
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_record_buffer_barrier_via_vtable(
                buffer,
                from_stage,
                to_stage,
                from_access,
                to_access,
            );
        }
        self.host_inner_mut().record_buffer_barrier(
            buffer,
            from_stage,
            to_stage,
            from_access,
            to_access,
        )
    }

    /// Copy image → buffer. See [`RhiCommandRecorderInner::record_copy_image_to_buffer`].
    ///
    /// Mode-routed; see [`Self::begin`] for the dispatch contract.
    /// **Cdylib path supports
    /// [`crate::core::rhi::StorageBuffer`]- and
    /// [`crate::core::rhi::PixelBuffer`]-flavored destinations
    /// today** — same buffer-flavor coverage as
    /// [`Self::record_buffer_barrier`].
    pub fn record_copy_image_to_buffer(
        &mut self,
        src: &Texture,
        src_layout: VulkanLayout,
        dst: &(impl VulkanBufferLike + ?Sized),
        region: ImageCopyRegion,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self
                .dispatch_record_copy_image_to_buffer_via_vtable(src, src_layout, dst, region);
        }
        self.host_inner_mut()
            .record_copy_image_to_buffer(src, src_layout, dst, region)
    }

    /// Copy buffer → image. Host-only until a cdylib consumer
    /// arrives; cdylib callers panic at [`Self::host_inner_mut`].
    pub fn record_copy_buffer_to_image(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &Texture,
        dst_layout: VulkanLayout,
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_copy_buffer_to_image(src, dst, dst_layout, region)
    }

    /// Compute dispatch.
    ///
    /// Mode-routed; see [`Self::begin`] for the dispatch contract.
    pub fn record_dispatch(
        &mut self,
        kernel: &VulkanComputeKernel,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_record_dispatch_via_vtable(kernel, group_x, group_y, group_z);
        }
        self.host_inner_mut()
            .record_dispatch(kernel, group_x, group_y, group_z)
    }

    /// Draw call.
    ///
    /// Mode-routed: host callers dispatch through `host_inner_mut`;
    /// cdylib callers dispatch through the v3 `record_draw` slot on
    /// [`RhiCommandRecorderMethodsVTable`](streamlib_plugin_abi::RhiCommandRecorderMethodsVTable).
    pub fn record_draw(
        &mut self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawCall,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_record_draw_via_vtable(kernel, frame_index, draw);
        }
        self.host_inner_mut().record_draw(kernel, frame_index, draw)
    }

    /// Indexed-draw variant.
    ///
    /// Mode-routed: host callers dispatch through `host_inner_mut`;
    /// cdylib callers dispatch through the v4 `record_draw_indexed`
    /// slot on
    /// [`RhiCommandRecorderMethodsVTable`](streamlib_plugin_abi::RhiCommandRecorderMethodsVTable).
    pub fn record_draw_indexed(
        &mut self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawIndexedCall,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_record_draw_indexed_via_vtable(kernel, frame_index, draw);
        }
        self.host_inner_mut()
            .record_draw_indexed(kernel, frame_index, draw)
    }

    /// Submit signaling a timeline semaphore.
    ///
    /// Mode-routed; see [`Self::begin`] for the dispatch contract.
    pub fn submit_signaling_timeline(
        &mut self,
        timeline: &HostVulkanTimelineSemaphore,
        signal_value: u64,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_submit_signaling_timeline_via_vtable(timeline, signal_value);
        }
        self.host_inner_mut()
            .submit_signaling_timeline(timeline, signal_value)
    }

    // -------------------------------------------------------------------------
    // Cdylib-mode dispatch helpers (Phase E sub-lift slice B — #984).
    // Each helper validates the methods vtable pointer, marshals
    // arguments into the wire-format integer types, dispatches
    // through the vtable, and converts the host's `i32 + err_buf`
    // return into `Result<()>`.
    // -------------------------------------------------------------------------

    fn dispatch_begin_via_vtable(&self) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "begin: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).begin)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_record_image_barrier_via_vtable(
        &self,
        texture: &Texture,
        from_layout: VulkanLayout,
        to_layout: VulkanLayout,
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_image_barrier: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).record_image_barrier)(
                self.handle,
                texture.handle,
                from_layout.0,
                to_layout.0,
                from_stage.0 as i64,
                to_stage.0 as i64,
                from_access.0 as i64,
                to_access.0 as i64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_record_buffer_barrier_via_vtable(
        &self,
        buffer: &(impl VulkanBufferLike + ?Sized),
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_buffer_barrier: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = if let Some(storage_handle) = buffer.cdylib_storage_buffer_handle() {
            unsafe {
                ((*self.methods_vtable).record_buffer_barrier)(
                    self.handle,
                    storage_handle,
                    from_stage.0 as i64,
                    to_stage.0 as i64,
                    from_access.0 as i64,
                    to_access.0 as i64,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            }
        } else if let Some(pixel_handle) = buffer.cdylib_pixel_buffer_handle() {
            unsafe {
                ((*self.methods_vtable).record_pixel_buffer_barrier)(
                    self.handle,
                    pixel_handle,
                    from_stage.0 as i64,
                    to_stage.0 as i64,
                    from_access.0 as i64,
                    to_access.0 as i64,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            }
        } else {
            return Err(Error::GpuError(
                "record_buffer_barrier: cdylib path only supports \
                 StorageBuffer- or PixelBuffer-flavored buffers today \
                 (extend the methods vtable with a sibling slot for other \
                 flavors)"
                    .into(),
            ));
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_record_dispatch_via_vtable(
        &self,
        kernel: &VulkanComputeKernel,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_dispatch: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).record_dispatch)(
                self.handle,
                kernel.handle,
                group_x,
                group_y,
                group_z,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_record_copy_image_to_buffer_via_vtable(
        &self,
        src: &Texture,
        src_layout: VulkanLayout,
        dst: &(impl VulkanBufferLike + ?Sized),
        region: ImageCopyRegion,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_copy_image_to_buffer: command recorder methods vtable is null".into(),
            ));
        }
        let region_repr = streamlib_plugin_abi::ImageCopyRegionRepr {
            width: region.width,
            height: region.height,
            buffer_offset: region.buffer_offset,
            buffer_row_length: region.buffer_row_length,
            buffer_image_height: region.buffer_image_height,
            mip_level: region.mip_level,
            array_layer: region.array_layer,
            _reserved_padding: 0,
        };
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = if let Some(dst_storage_handle) = dst.cdylib_storage_buffer_handle() {
            unsafe {
                ((*self.methods_vtable).record_copy_image_to_buffer)(
                    self.handle,
                    src.handle,
                    src_layout.0,
                    dst_storage_handle,
                    &region_repr,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            }
        } else if let Some(dst_pixel_handle) = dst.cdylib_pixel_buffer_handle() {
            unsafe {
                ((*self.methods_vtable).record_copy_image_to_pixel_buffer)(
                    self.handle,
                    src.handle,
                    src_layout.0,
                    dst_pixel_handle,
                    &region_repr,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            }
        } else {
            return Err(Error::GpuError(
                "record_copy_image_to_buffer: cdylib path only supports \
                 StorageBuffer- or PixelBuffer-flavored destinations today \
                 (extend the methods vtable with a sibling slot for other \
                 flavors)"
                    .into(),
            ));
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_submit_signaling_timeline_via_vtable(
        &self,
        timeline: &HostVulkanTimelineSemaphore,
        signal_value: u64,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "submit_signaling_timeline: command recorder methods vtable is null".into(),
            ));
        }
        let timeline_handle = timeline as *const HostVulkanTimelineSemaphore as *const c_void;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).submit_signaling_timeline)(
                self.handle,
                timeline_handle,
                signal_value,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Cdylib-side dispatch helper for the v5 `submit` slot.
    fn dispatch_submit_via_vtable(&self) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "submit: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).submit)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Cdylib-side dispatch helper for the v5 `submit_and_wait` slot.
    fn dispatch_submit_and_wait_via_vtable(&self) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "submit_and_wait: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).submit_and_wait)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Submit without semaphore signaling.
    ///
    /// Mode-routed: host callers dispatch through `host_inner_mut`;
    /// cdylib callers dispatch through the v5 `submit` slot on
    /// [`RhiCommandRecorderMethodsVTable`](streamlib_plugin_abi::RhiCommandRecorderMethodsVTable).
    pub fn submit(&mut self) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_submit_via_vtable();
        }
        self.host_inner_mut().submit()
    }

    /// Submit and block until the GPU completes.
    ///
    /// Mode-routed: host callers dispatch through `host_inner_mut`;
    /// cdylib callers dispatch through the v5 `submit_and_wait` slot
    /// on [`RhiCommandRecorderMethodsVTable`](streamlib_plugin_abi::RhiCommandRecorderMethodsVTable).
    /// `RhiToneMapper::apply_with_layouts` is the first in-tree
    /// cdylib consumer.
    pub fn submit_and_wait(&mut self) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_submit_and_wait_via_vtable();
        }
        self.host_inner_mut().submit_and_wait()
    }

    /// Engine-internal submit path supporting binary + timeline waits.
    /// **Engine-internal** — for `VulkanPresentTarget`'s render submit.
    ///
    /// Mode-routed: host callers dispatch through `host_inner_mut`;
    /// cdylib callers dispatch through the v3
    /// `submit_with_semaphores` slot.
    pub(crate) fn submit_with_semaphores(
        &mut self,
        waits: &[vk::SemaphoreSubmitInfo],
        signals: &[vk::SemaphoreSubmitInfo],
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_submit_with_semaphores_via_vtable(waits, signals);
        }
        self.host_inner_mut().submit_with_semaphores(waits, signals)
    }

    /// Engine-internal swapchain-image layout transition.
    /// **Engine-internal** — for `VulkanPresentTarget`'s pre/post-draw
    /// barriers. Mode-routed: host callers dispatch through
    /// `host_inner_mut`; cdylib callers dispatch through the v3
    /// `record_swapchain_image_barrier` slot.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_swapchain_image_barrier(
        &mut self,
        image: vk::Image,
        old_layout: vk::ImageLayout,
        new_layout: vk::ImageLayout,
        src_stage: vk::PipelineStageFlags2,
        src_access: vk::AccessFlags2,
        dst_stage: vk::PipelineStageFlags2,
        dst_access: vk::AccessFlags2,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_record_swapchain_image_barrier_via_vtable(
                image, old_layout, new_layout, src_stage, src_access, dst_stage, dst_access,
            );
        }
        self.host_inner_mut().record_swapchain_image_barrier(
            image, old_layout, new_layout, src_stage, src_access, dst_stage, dst_access,
        )
    }

    /// Engine-internal dynamic-rendering begin.
    /// **Engine-internal** — for
    /// [`PresentFrame::begin_rendering`](super::vulkan_present_target::PresentFrame::begin_rendering).
    /// Mode-routed: host callers dispatch through `host_inner_mut`;
    /// cdylib callers dispatch through the v3
    /// `cmd_begin_dynamic_rendering` slot.
    pub(crate) fn cmd_begin_dynamic_rendering(
        &mut self,
        image_view: vk::ImageView,
        extent: (u32, u32),
        clear_color: Option<[f32; 4]>,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_cmd_begin_dynamic_rendering_via_vtable(
                image_view,
                extent,
                clear_color,
            );
        }
        self.host_inner_mut()
            .cmd_begin_dynamic_rendering(image_view, extent, clear_color)
    }

    /// Engine-internal dynamic-rendering end.
    /// **Engine-internal** — for
    /// [`PresentFrame::end_rendering`](super::vulkan_present_target::PresentFrame::end_rendering).
    /// Mode-routed: host callers dispatch through `host_inner_mut`;
    /// cdylib callers dispatch through the v3
    /// `cmd_end_dynamic_rendering` slot.
    pub(crate) fn cmd_end_dynamic_rendering(&mut self) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_cmd_end_dynamic_rendering_via_vtable();
        }
        self.host_inner_mut().cmd_end_dynamic_rendering()
    }

    // -------------------------------------------------------------------------
    // v3 dispatch helpers (#1066) — match the v1 / v2 helpers above.
    // -------------------------------------------------------------------------

    fn dispatch_record_draw_via_vtable(
        &self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawCall,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_draw: command recorder methods vtable is null".into(),
            ));
        }
        let (viewport_present, vp) = match draw.viewport {
            Some(v) => (1u32, v),
            None => (0u32, Viewport::full(0, 0)),
        };
        let (scissor_present, sc) = match draw.scissor {
            Some(s) => (1u32, s),
            None => (0u32, ScissorRect::full(0, 0)),
        };
        let draw_repr = streamlib_plugin_abi::DrawCallRepr {
            vertex_count: draw.vertex_count,
            instance_count: draw.instance_count,
            first_vertex: draw.first_vertex,
            first_instance: draw.first_instance,
            viewport_present,
            scissor_present,
            viewport: streamlib_plugin_abi::ViewportRepr {
                x: vp.x,
                y: vp.y,
                width: vp.width,
                height: vp.height,
                min_depth: vp.min_depth,
                max_depth: vp.max_depth,
            },
            scissor: streamlib_plugin_abi::ScissorRectRepr {
                x: sc.x,
                y: sc.y,
                width: sc.width,
                height: sc.height,
            },
        };
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).record_draw)(
                self.handle,
                kernel.handle,
                frame_index,
                &draw_repr,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_record_draw_indexed_via_vtable(
        &self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawIndexedCall,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_draw_indexed: command recorder methods vtable is null".into(),
            ));
        }
        let (viewport_present, vp) = match draw.viewport {
            Some(v) => (1u32, v),
            None => (0u32, Viewport::full(0, 0)),
        };
        let (scissor_present, sc) = match draw.scissor {
            Some(s) => (1u32, s),
            None => (0u32, ScissorRect::full(0, 0)),
        };
        let draw_repr = streamlib_plugin_abi::DrawIndexedCallRepr {
            index_count: draw.index_count,
            instance_count: draw.instance_count,
            first_index: draw.first_index,
            vertex_offset: draw.vertex_offset,
            first_instance: draw.first_instance,
            viewport_present,
            scissor_present,
            _reserved_padding: 0,
            viewport: streamlib_plugin_abi::ViewportRepr {
                x: vp.x,
                y: vp.y,
                width: vp.width,
                height: vp.height,
                min_depth: vp.min_depth,
                max_depth: vp.max_depth,
            },
            scissor: streamlib_plugin_abi::ScissorRectRepr {
                x: sc.x,
                y: sc.y,
                width: sc.width,
                height: sc.height,
            },
        };
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).record_draw_indexed)(
                self.handle,
                kernel.handle,
                frame_index,
                &draw_repr,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_record_swapchain_image_barrier_via_vtable(
        &self,
        image: vk::Image,
        old_layout: vk::ImageLayout,
        new_layout: vk::ImageLayout,
        src_stage: vk::PipelineStageFlags2,
        src_access: vk::AccessFlags2,
        dst_stage: vk::PipelineStageFlags2,
        dst_access: vk::AccessFlags2,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_swapchain_image_barrier: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).record_swapchain_image_barrier)(
                self.handle,
                image.as_raw(),
                old_layout.as_raw(),
                new_layout.as_raw(),
                src_stage.bits() as i64,
                dst_stage.bits() as i64,
                src_access.bits() as i64,
                dst_access.bits() as i64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_cmd_begin_dynamic_rendering_via_vtable(
        &self,
        image_view: vk::ImageView,
        extent: (u32, u32),
        clear_color: Option<[f32; 4]>,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "cmd_begin_dynamic_rendering: command recorder methods vtable is null".into(),
            ));
        }
        let has_clear = if clear_color.is_some() { 1u32 } else { 0u32 };
        let rgba = clear_color.unwrap_or([0.0; 4]);
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).cmd_begin_dynamic_rendering)(
                self.handle,
                image_view.as_raw(),
                extent.0,
                extent.1,
                has_clear,
                rgba[0],
                rgba[1],
                rgba[2],
                rgba[3],
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_cmd_end_dynamic_rendering_via_vtable(&self) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "cmd_end_dynamic_rendering: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).cmd_end_dynamic_rendering)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    fn dispatch_submit_with_semaphores_via_vtable(
        &self,
        waits: &[vk::SemaphoreSubmitInfo],
        signals: &[vk::SemaphoreSubmitInfo],
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "submit_with_semaphores: command recorder methods vtable is null".into(),
            ));
        }
        // Project the vk::SemaphoreSubmitInfo arrays into POD reprs.
        let waits_repr: Vec<streamlib_plugin_abi::SemaphoreSubmitInfoRepr> = waits
            .iter()
            .map(|w| streamlib_plugin_abi::SemaphoreSubmitInfoRepr {
                semaphore: w.semaphore.as_raw(),
                value: w.value,
                stage_mask: w.stage_mask.bits() as u64,
                device_index: w.device_index,
                _reserved_padding: 0,
            })
            .collect();
        let signals_repr: Vec<streamlib_plugin_abi::SemaphoreSubmitInfoRepr> = signals
            .iter()
            .map(|s| streamlib_plugin_abi::SemaphoreSubmitInfoRepr {
                semaphore: s.semaphore.as_raw(),
                value: s.value,
                stage_mask: s.stage_mask.bits() as u64,
                device_index: s.device_index,
                _reserved_padding: 0,
            })
            .collect();
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).submit_with_semaphores)(
                self.handle,
                if waits_repr.is_empty() {
                    std::ptr::null()
                } else {
                    waits_repr.as_ptr()
                },
                waits_repr.len(),
                if signals_repr.is_empty() {
                    std::ptr::null()
                } else {
                    signals_repr.as_ptr()
                },
                signals_repr.len(),
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }
}

impl Drop for RhiCommandRecorder {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with `Box::into_raw` in `from_inner`.
            unsafe {
                ((*self.vtable).drop_command_recorder)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for RhiCommandRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiCommandRecorder").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn rhi_command_recorder_layout() {
        // Phase E sub-lift slice B (#984) appended `methods_vtable`
        // (16 → 24 bytes); the PluginAbiObject now mirrors the
        // `(handle, vtable, methods_vtable)` triple used by every
        // per-type kernel PluginAbiObject and `RhiColorConverter`.
        assert_eq!(size_of::<RhiCommandRecorder>(), 24);
        assert_eq!(align_of::<RhiCommandRecorder>(), 8);
        assert_eq!(offset_of!(RhiCommandRecorder, handle), 0);
        assert_eq!(offset_of!(RhiCommandRecorder, vtable), 8);
        assert_eq!(offset_of!(RhiCommandRecorder, methods_vtable), 16);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor, PixelBuffer, PixelFormat};
    use crate::vulkan::rhi::HostVulkanBuffer;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    fn make_storage_buffer(device: &Arc<HostVulkanDevice>, element_count: u32) -> PixelBuffer {
        let vk_buf =
            HostVulkanBuffer::new(device, (element_count as u64) * 4).expect("storage buffer");
        PixelBuffer::from_host_vulkan_buffer(
            Arc::new(vk_buf),
            element_count,
            1,
            4,
            crate::core::rhi::PixelFormat::Bgra32,
        )
    }

    fn write_buffer_u32(buf: &PixelBuffer, values: &[u32]) {
        let ptr = buf.buffer_ref().inner.mapped_ptr() as *mut u32;
        unsafe {
            std::ptr::copy_nonoverlapping(values.as_ptr(), ptr, values.len());
        }
    }

    fn read_buffer_u32(buf: &PixelBuffer, len: usize) -> Vec<u32> {
        let ptr = buf.buffer_ref().inner.mapped_ptr() as *const u32;
        let mut out = vec![0u32; len];
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), len);
        }
        out
    }

    // ----- State-machine tests (run on hardware; bail without GPU) -----

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
    )]
    #[test]
    fn record_before_begin_is_typed_error() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let mut rec = RhiCommandRecorder::new(&device, "no-begin").expect("create");
        let buf = make_storage_buffer(&device, 4);
        let err = rec
            .record_buffer_barrier(
                &buf,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::HOST,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::HOST_READ,
            )
            .err()
            .expect("expected typed error");
        let msg = format!("{err}");
        assert!(msg.contains("outside an active recording"), "got: {msg}");
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
    )]
    #[test]
    fn double_begin_is_typed_error() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let mut rec = RhiCommandRecorder::new(&device, "double-begin").expect("create");
        rec.begin().expect("first begin");
        let err = rec.begin().err().expect("expected typed error");
        let msg = format!("{err}");
        assert!(msg.contains("already in progress"), "got: {msg}");
        // submit so Drop cleans up cleanly.
        rec.submit_and_wait().expect("submit_and_wait");
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
    )]
    #[test]
    fn submit_without_begin_is_typed_error() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let mut rec = RhiCommandRecorder::new(&device, "no-begin-submit").expect("create");
        let err = rec.submit().err().expect("expected typed error");
        let msg = format!("{err}");
        assert!(msg.contains("without an active recording"), "got: {msg}");
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
    )]
    #[test]
    fn first_begin_does_not_block_on_unsignaled_fence() {
        // `submission_in_flight=false` at construction must gate the
        // fence wait — otherwise the first `begin()` would block forever
        // on the unsignaled fence (the fence is created without
        // `SIGNALED` flag now, since the gate makes pre-signaling
        // unnecessary). Mentally revert the gate: the wait_for_fences
        // call at begin() blocks indefinitely and this test hangs.
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let mut rec = RhiCommandRecorder::new(&device, "first-begin-no-block").expect("create");
        rec.begin()
            .expect("first begin must not block on unsignaled fence");
        rec.submit_and_wait().expect("submit_and_wait");
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
    )]
    #[test]
    fn begin_after_submit_succeeds() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let mut rec = RhiCommandRecorder::new(&device, "begin-after-submit").expect("create");
        rec.begin().expect("begin 1");
        rec.submit_and_wait().expect("submit_and_wait");
        // Second begin should pick up the now-Idle state.
        rec.begin().expect("begin 2");
        rec.submit_and_wait().expect("submit_and_wait 2");
    }

    // ----- Vulkan-free: dispatcher argument-ordering regression test (#1089) -----
    //
    // The plugin-ABI vtable's `record_swapchain_image_barrier` slot
    // groups parameters by kind: (from_stage, to_stage, from_access,
    // to_access). The dispatcher in `dispatch_record_swapchain_image_barrier_via_vtable`
    // received them as a (src_stage, src_access, dst_stage, dst_access)
    // tuple from the caller and must reorder them onto the wire. Issue
    // #1089 was a regression where the dispatcher passed them in
    // (src_stage, src_access, dst_stage, dst_access) order on the wire —
    // misaligned by one slot — so the host wrapper interpreted the
    // caller's dst_stage bits as src_access (and vice versa). The bit
    // values collide on real Vulkan flag layouts
    // (`COLOR_ATTACHMENT_OUTPUT` stage = `DEPTH_STENCIL_ATTACHMENT_WRITE`
    // access = 0x400), which is what made the validation layer flag a
    // missing-stage error rather than a bogus-bits error.
    //
    // Mentally reverting the swap reproduces #1089's exact symptom in
    // this test: each captured slot would contain the wrong caller-side
    // value.

    use std::cell::Cell;
    use std::ffi::c_void;

    thread_local! {
        static CAPTURED_BARRIER_ARGS:
            Cell<Option<[i64; 4]>> = const { Cell::new(None) };
    }

    unsafe extern "C" fn capture_record_swapchain_image_barrier(
        _recorder_handle: *const c_void,
        _image_raw: u64,
        _from_layout_raw: i32,
        _to_layout_raw: i32,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        _err_buf: *mut u8,
        _err_buf_cap: usize,
        _err_len: *mut usize,
    ) -> i32 {
        CAPTURED_BARRIER_ARGS.with(|c| {
            c.set(Some([
                from_stage_raw,
                to_stage_raw,
                from_access_raw,
                to_access_raw,
            ]));
        });
        0
    }

    // Fill the remaining slots with stub functions that just return 0;
    // the test only exercises the swapchain-barrier slot.
    macro_rules! stub_zero {
        ($name:ident($($arg:ident: $ty:ty),* $(,)?)) => {
            unsafe extern "C" fn $name($($arg: $ty),*) -> i32 {
                $(let _ = $arg;)*
                0
            }
        };
    }
    stub_zero!(stub_begin(a: *const c_void, b: *mut u8, c: usize, d: *mut usize));
    stub_zero!(stub_image_barrier(
        a: *const c_void, b: *const c_void, c: i32, d: i32, e: i64, f: i64, g: i64, h: i64,
        i: *mut u8, j: usize, k: *mut usize,
    ));
    stub_zero!(stub_buffer_barrier(
        a: *const c_void, b: *const c_void, c: i64, d: i64, e: i64, f: i64,
        g: *mut u8, h: usize, i: *mut usize,
    ));
    stub_zero!(stub_dispatch(
        a: *const c_void, b: *const c_void, c: u32, d: u32, e: u32,
        f: *mut u8, g: usize, h: *mut usize,
    ));
    stub_zero!(stub_copy_image_to_buffer(
        a: *const c_void, b: *const c_void, c: i32, d: *const c_void,
        e: *const streamlib_plugin_abi::ImageCopyRegionRepr,
        f: *mut u8, g: usize, h: *mut usize,
    ));
    stub_zero!(stub_submit_signaling_timeline(
        a: *const c_void, b: *const c_void, c: u64, d: *mut u8, e: usize, f: *mut usize,
    ));
    stub_zero!(stub_begin_dynamic_rendering(
        a: *const c_void, b: u64, c: u32, d: u32, e: u32,
        f: f32, g: f32, h: f32, i: f32, j: *mut u8, k: usize, l: *mut usize,
    ));
    stub_zero!(stub_end_dynamic_rendering(a: *const c_void, b: *mut u8, c: usize, d: *mut usize));
    stub_zero!(stub_submit_with_semaphores(
        a: *const c_void,
        b: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr, c: usize,
        d: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr, e: usize,
        f: *mut u8, g: usize, h: *mut usize,
    ));
    stub_zero!(stub_record_draw(
        a: *const c_void, b: *const c_void, c: u32,
        d: *const streamlib_plugin_abi::DrawCallRepr,
        e: *mut u8, f: usize, g: *mut usize,
    ));
    stub_zero!(stub_record_draw_indexed(
        a: *const c_void, b: *const c_void, c: u32,
        d: *const streamlib_plugin_abi::DrawIndexedCallRepr,
        e: *mut u8, f: usize, g: *mut usize,
    ));
    stub_zero!(stub_submit(a: *const c_void, b: *mut u8, c: usize, d: *mut usize));
    stub_zero!(stub_submit_and_wait(a: *const c_void, b: *mut u8, c: usize, d: *mut usize));

    #[test]
    fn dispatch_swapchain_barrier_passes_args_in_vtable_slot_order() {
        // Use distinctive non-colliding bit patterns for each role so a
        // misaligned wire reordering would produce visibly wrong slots.
        let src_stage = vk::PipelineStageFlags2::COMPUTE_SHADER; // bit 11 (0x800)
        let dst_stage = vk::PipelineStageFlags2::FRAGMENT_SHADER; // bit 7 (0x80)
        let src_access = vk::AccessFlags2::SHADER_STORAGE_READ; // bit 33 (0x200000000)
        let dst_access = vk::AccessFlags2::SHADER_SAMPLED_READ; // bit 32 (0x100000000)

        // Fake methods vtable with our capture function in the swapchain slot.
        let fake_vtable = streamlib_plugin_abi::RhiCommandRecorderMethodsVTable {
            layout_version:
                streamlib_plugin_abi::RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION,
            _reserved_padding: 0,
            begin: stub_begin,
            record_image_barrier: stub_image_barrier,
            record_buffer_barrier: stub_buffer_barrier,
            record_dispatch: stub_dispatch,
            record_copy_image_to_buffer: stub_copy_image_to_buffer,
            submit_signaling_timeline: stub_submit_signaling_timeline,
            record_pixel_buffer_barrier: stub_buffer_barrier,
            record_copy_image_to_pixel_buffer: stub_copy_image_to_buffer,
            record_swapchain_image_barrier: capture_record_swapchain_image_barrier,
            cmd_begin_dynamic_rendering: stub_begin_dynamic_rendering,
            cmd_end_dynamic_rendering: stub_end_dynamic_rendering,
            submit_with_semaphores: stub_submit_with_semaphores,
            record_draw: stub_record_draw,
            record_draw_indexed: stub_record_draw_indexed,
            submit: stub_submit,
            submit_and_wait: stub_submit_and_wait,
        };

        // Synthetic PluginAbiObject with our fake vtable. handle/vtable can stay null
        // because the capture function never dereferences them.
        let beta = RhiCommandRecorder {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            methods_vtable: &fake_vtable,
        };

        CAPTURED_BARRIER_ARGS.with(|c| c.set(None));
        let rc = beta.dispatch_record_swapchain_image_barrier_via_vtable(
            vk::Image::null(),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            src_stage,
            src_access,
            dst_stage,
            dst_access,
        );
        assert!(rc.is_ok(), "dispatcher returned error: {:?}", rc);

        let captured = CAPTURED_BARRIER_ARGS
            .with(|c| c.get())
            .expect("vtable slot did not fire");

        // The vtable signature names these parameters (in order):
        // from_stage_raw, to_stage_raw, from_access_raw, to_access_raw.
        // The caller's (src_stage, src_access, dst_stage, dst_access)
        // must therefore reach the vtable as (src, dst, src, dst) —
        // src→from, dst→to. Mentally reverting the dispatcher's swap
        // makes this assertion fail with src_access in the to_stage
        // slot (#1089's exact failure mode).
        assert_eq!(
            captured[0],
            src_stage.bits() as i64,
            "from_stage_raw must receive caller's src_stage"
        );
        assert_eq!(
            captured[1],
            dst_stage.bits() as i64,
            "to_stage_raw must receive caller's dst_stage (#1089: was src_access)"
        );
        assert_eq!(
            captured[2],
            src_access.bits() as i64,
            "from_access_raw must receive caller's src_access (#1089: was dst_stage)"
        );
        assert_eq!(
            captured[3],
            dst_access.bits() as i64,
            "to_access_raw must receive caller's dst_access"
        );
    }

    // ----- Integration test: real compute kernel + timeline-signaling submit -----
    //
    // Mirrors the camera-shape pattern: compute dispatch into a storage
    // buffer, post-compute buffer barrier so HOST reads see the result,
    // submit signaling a timeline at value N, wait on the timeline,
    // verify mapped contents.

    fn blend_descriptor(input_count: u32) -> Vec<ComputeBindingSpec> {
        let mut bindings: Vec<ComputeBindingSpec> = (0..input_count)
            .map(ComputeBindingSpec::storage_buffer)
            .collect();
        bindings.push(ComputeBindingSpec::storage_buffer(8));
        bindings
    }

    fn blend_spv(input_count: u32) -> &'static [u8] {
        match input_count {
            1 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv")),
            2 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_2.spv")),
            _ => panic!("unexpected input_count for record-test fixture"),
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
    )]
    #[test]
    fn dispatch_via_recorder_matches_direct_kernel_dispatch() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let element_count = 256u32;
        let bindings = blend_descriptor(2);
        let kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "recorder-test",
                spv: blend_spv(2),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel");

        let input_a = make_storage_buffer(&device, element_count);
        let input_b = make_storage_buffer(&device, element_count);
        let output = make_storage_buffer(&device, element_count);

        let pattern_a: Vec<u32> = (0..element_count).map(|i| i + 1).collect();
        let pattern_b: Vec<u32> = (0..element_count).map(|i| (i + 1) * 2).collect();
        write_buffer_u32(&input_a, &pattern_a);
        write_buffer_u32(&input_b, &pattern_b);

        kernel.set_storage_buffer_pixel(0, &input_a).expect("set 0");
        kernel.set_storage_buffer_pixel(1, &input_b).expect("set 1");
        kernel.set_storage_buffer_pixel(8, &output).expect("set 8");
        let push: [u32; 1] = [element_count];
        kernel
            .set_push_constants_value(&push)
            .expect("push constants");

        let timeline = HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline");

        let mut rec = RhiCommandRecorder::new(&device, "recorder-test").expect("recorder");
        rec.begin().expect("begin");
        let group_count_x = element_count.div_ceil(64);
        rec.record_dispatch(&kernel, group_count_x, 1, 1)
            .expect("dispatch");
        rec.record_buffer_barrier(
            &output,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::HOST,
            VulkanAccess::SHADER_WRITE,
            VulkanAccess::HOST_READ,
        )
        .expect("post-dispatch barrier");
        rec.submit_signaling_timeline(&timeline, 1).expect("submit");

        timeline.wait(1, u64::MAX).expect("wait");

        let actual = read_buffer_u32(&output, element_count as usize);
        let expected: Vec<u32> = (0..element_count as usize)
            .map(|i| pattern_a[i] + pattern_b[i])
            .collect();
        assert_eq!(actual, expected, "recorder dispatch result mismatch");
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
    )]
    #[test]
    fn back_to_back_recordings_signal_distinct_timeline_values() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let element_count = 64u32;
        let bindings = blend_descriptor(1);
        let kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "recorder-back-to-back",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel");

        let input = make_storage_buffer(&device, element_count);
        let output = make_storage_buffer(&device, element_count);
        let pattern: Vec<u32> = (0..element_count).map(|i| i + 1).collect();
        write_buffer_u32(&input, &pattern);

        let timeline = HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline");
        let mut rec = RhiCommandRecorder::new(&device, "recorder-back-to-back").expect("recorder");

        for frame in 1..=3u64 {
            kernel.set_storage_buffer_pixel(0, &input).expect("set 0");
            kernel.set_storage_buffer_pixel(8, &output).expect("set 8");
            let push: [u32; 1] = [element_count];
            kernel.set_push_constants_value(&push).expect("push");

            rec.begin().expect("begin");
            let group_count_x = element_count.div_ceil(64);
            rec.record_dispatch(&kernel, group_count_x, 1, 1)
                .expect("dispatch");
            rec.record_buffer_barrier(
                &output,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::HOST,
                VulkanAccess::SHADER_WRITE,
                VulkanAccess::HOST_READ,
            )
            .expect("barrier");
            rec.submit_signaling_timeline(&timeline, frame)
                .expect("submit");

            timeline.wait(frame, u64::MAX).expect("wait");
            assert_eq!(
                timeline.current_value().expect("current_value"),
                frame,
                "timeline counter mismatch after frame {frame}"
            );
        }
    }
}
