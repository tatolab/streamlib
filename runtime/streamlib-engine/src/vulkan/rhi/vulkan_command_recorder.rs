// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-owned multi-step command-buffer recorder.

use std::ffi::c_void;
use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{DrawCall, DrawIndexedCall, Texture, VulkanLayout};
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

/// Tracks whether the recorder's command buffer is currently inside an
/// open dynamic-rendering instance (a `cmd_begin_rendering` without a
/// matching `cmd_end_rendering`).
///
/// [`VulkanPresentTarget::end_frame`](super::vulkan_present_target::VulkanPresentTarget::end_frame)
/// reads this before recording the swapchain post-draw layout barrier: a
/// `vkCmdPipelineBarrier2` recorded inside an open dynamic-rendering
/// instance is `VUID-vkCmdPipelineBarrier2`-class undefined behavior, so a
/// caller that drove `begin_frame` and forgot
/// `cmd_end_dynamic_rendering` must have its pass auto-closed first. Pure
/// state (no Vulkan handle) so the open/close balance is unit-testable
/// without a GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecorderDynamicRenderingBalance {
    is_open: bool,
}

impl RecorderDynamicRenderingBalance {
    /// Whether a dynamic-rendering pass is currently open on the recorder.
    pub(crate) fn is_open(self) -> bool {
        self.is_open
    }

    /// Record that `cmd_begin_dynamic_rendering` opened a pass.
    pub(crate) fn mark_opened(&mut self) {
        self.is_open = true;
    }

    /// Record that the pass closed (`cmd_end_dynamic_rendering`, a fresh
    /// `begin()`, or an aborted recording).
    pub(crate) fn mark_closed(&mut self) {
        self.is_open = false;
    }
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
    state: Mutex<RecorderState>,
    /// Whether a dynamic-rendering pass is currently open on this
    /// recorder's command buffer. Set by
    /// [`Self::cmd_begin_dynamic_rendering`], cleared by
    /// [`Self::cmd_end_dynamic_rendering`], [`Self::begin`], and
    /// [`Self::abort_recording`]. Read by
    /// [`VulkanPresentTarget::end_frame`](super::vulkan_present_target::VulkanPresentTarget::end_frame)
    /// so a still-open pass is closed before the post-draw layout barrier
    /// (a barrier inside an open dynamic-rendering instance is UB).
    render_pass_balance: RecorderDynamicRenderingBalance,
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
            render_pass_balance: RecorderDynamicRenderingBalance::default(),
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
                self.vulkan_device.wait_for_fences_blocking_counted(
                    &[self.completion_fence],
                    format_args!("RhiCommandRecorder '{}' at begin()", self.label),
                )?;
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

        // A fresh recording starts with no dynamic-rendering pass open —
        // the reset command buffer above discarded any prior pass.
        self.render_pass_balance.mark_closed();
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

    /// Record `vkCmdCopyBuffer` over the first `byte_size` bytes of both
    /// buffers.
    pub fn record_copy_buffer_to_buffer(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &(impl VulkanBufferLike + ?Sized),
        byte_size: u64,
    ) -> Result<()> {
        self.expect_recording("record_copy_buffer_to_buffer")?;
        if byte_size > src.vk_buffer_size() || byte_size > dst.vk_buffer_size() {
            return Err(Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_buffer_to_buffer: copy of {} bytes exceeds \
                 src ({}) or dst ({})",
                self.label,
                byte_size,
                src.vk_buffer_size(),
                dst.vk_buffer_size(),
            )));
        }
        let region = vk::BufferCopy::builder()
            .src_offset(0)
            .dst_offset(0)
            .size(byte_size as vk::DeviceSize)
            .build();
        unsafe {
            self.device.cmd_copy_buffer(
                self.command_buffer,
                src.vk_buffer(),
                dst.vk_buffer(),
                &[region],
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

    /// End recording and submit in one queue submission, optionally
    /// GPU-waiting `wait` (`(timeline, wait_value)`) before the recorded
    /// work runs and optionally signaling `signal`
    /// (`(timeline, signal_value)`) on completion. `None` for either side
    /// omits that half.
    ///
    /// This backs the OPAQUE_FD/CUDA per-frame producer copy
    /// (`copy_texture_to_storage_buffer_and_signal`): the producer waits
    /// on the consumer's `consume_done` timeline before overwriting the
    /// shared buffer, then signals `produce_done` after the copy so the
    /// consumer's `acquire_read` unblocks. Each signal/wait value MUST be
    /// strictly greater than its timeline's current counter (Vulkan
    /// disallows monotonic regressions).
    ///
    /// When both sides are `None` the caller gets a bare submission with
    /// no host wait — use [`Self::submit_and_wait`] if a host-side drain
    /// is needed instead.
    #[tracing::instrument(level = "trace", skip(self, wait, signal), fields(label = %self.label))]
    pub fn submit_waiting_and_signaling_timeline(
        &mut self,
        wait: Option<(&HostVulkanTimelineSemaphore, u64)>,
        signal: Option<(&HostVulkanTimelineSemaphore, u64)>,
    ) -> Result<()> {
        let waits: Vec<vk::SemaphoreSubmitInfo> = wait
            .into_iter()
            .map(|(timeline, value)| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(timeline.semaphore())
                    .value(value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build()
            })
            .collect();
        let signals: Vec<vk::SemaphoreSubmitInfo> = signal
            .into_iter()
            .map(|(timeline, value)| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(timeline.semaphore())
                    .value(value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build()
            })
            .collect();
        self.submit_with_semaphores(&waits, &signals)
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
        self.drain_completion_fence("submit_and_wait")
    }

    /// Host-block until this recorder's most recent submission drains,
    /// via its internal completion fence. Use after a
    /// [`Self::submit_waiting_and_signaling_timeline`] whose signal side
    /// is `None`: with no GPU-side signal to order a downstream consumer,
    /// a host-side drain is the only completion guarantee the caller has.
    /// No-op when nothing is in flight (the fence is already signaled).
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub fn wait_for_completion(&mut self) -> Result<()> {
        self.drain_completion_fence("wait_for_completion")
    }

    /// Wait out the in-flight submission, then reset the fence and clear the
    /// in-flight flag so the next `begin()` has nothing left to wait for.
    ///
    /// Reclaiming here rather than leaving it to `begin()` is what makes a
    /// caller that waits explicitly pay one stall per submission instead of
    /// two: without it the fence stays marked in flight, and the next
    /// `begin()` blocks a second time on a fence that is already signaled.
    fn drain_completion_fence(&mut self, waited_in: &'static str) -> Result<()> {
        if !self.submission_in_flight {
            return Ok(());
        }
        self.vulkan_device.wait_for_fences_blocking_counted(
            &[self.completion_fence],
            format_args!("RhiCommandRecorder '{}' in {waited_in}", self.label),
        )?;
        unsafe {
            self.device
                .reset_fences(&[self.completion_fence])
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': reset_fences in {waited_in}: {e}",
                        self.label
                    ))
                })?;
        }
        self.submission_in_flight = false;
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
    /// `Texture` handle; this variant is used by
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
        self.render_pass_balance.mark_opened();
        Ok(())
    }

    /// Close the dynamic-rendering pass opened by
    /// [`Self::cmd_begin_dynamic_rendering`].
    pub(crate) fn cmd_end_dynamic_rendering(&mut self) -> Result<()> {
        self.expect_recording("cmd_end_dynamic_rendering")?;
        unsafe {
            self.device.cmd_end_rendering(self.command_buffer);
        }
        self.render_pass_balance.mark_closed();
        Ok(())
    }

    /// Whether a dynamic-rendering pass is currently open on this
    /// recorder's command buffer (a `cmd_begin_dynamic_rendering` without
    /// a matching `cmd_end_dynamic_rendering`). Read by
    /// [`VulkanPresentTarget::end_frame`](super::vulkan_present_target::VulkanPresentTarget::end_frame)
    /// to close a forgotten pass before the post-draw layout barrier.
    pub(crate) fn in_render_pass(&self) -> bool {
        self.render_pass_balance.is_open()
    }

    /// Abandon an in-progress recording without submitting: reset the
    /// state machine to [`RecorderState::Idle`] and drop any open
    /// dynamic-rendering pass so the next [`Self::begin`] starts clean.
    /// The begun-but-unsubmitted command buffer is discarded at that
    /// `begin()`'s `reset_command_buffer`. Used when a frame is abandoned
    /// after `begin()` but before submit (e.g. swapchain `OUT_OF_DATE_KHR`
    /// on acquire, or a failed per-frame record), so the recorder slot is
    /// reusable next attempt. Harmless when nothing is recording.
    pub fn abort_recording(&mut self) {
        self.render_pass_balance.mark_closed();
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

// =============================================================================
// Public handle
// =============================================================================

/// Multi-step command-buffer recorder.
///
/// The opaque handle points at a `Box<RhiCommandRecorderInner>`; `Drop`
/// does `Box::from_raw` + drop.
///
/// **Single-owner; deliberately NOT `Clone`.** Recording carries
/// mutable state (`begin()` → `record_*(&mut self)` → `submit_*(&mut
/// self)`) that doesn't survive duplication. Calling `.clone()` on the
/// public handle is a compile error, locked by the `compile_fail`
/// doctest below:
///
/// ```compile_fail
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<streamlib_engine::vulkan::rhi::RhiCommandRecorder>();
/// ```
pub struct RhiCommandRecorder {
    /// Opaque handle to the host's `Box<RhiCommandRecorderInner>`.
    pub(crate) handle: *const c_void,
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
    /// opaque handle.
    pub(crate) fn from_inner(inner: RhiCommandRecorderInner) -> Self {
        let handle = Box::into_raw(Box::new(inner)) as *const c_void;
        Self { handle }
    }

    /// Engine-internal mutable borrow of the host-owned
    /// `RhiCommandRecorderInner`.
    pub(crate) fn host_inner_mut(&mut self) -> &mut RhiCommandRecorderInner {
        // SAFETY: `self.handle` is `Box::into_raw(Box<RhiCommandRecorderInner>)`
        // and `&mut self` guarantees no other reference exists.
        unsafe { &mut *(self.handle as *mut RhiCommandRecorderInner) }
    }

    /// Host-side render-pass balance read used by
    /// [`VulkanPresentTarget::end_frame`](super::vulkan_present_target::VulkanPresentTarget::end_frame):
    /// is the backing recorder inside an open dynamic-rendering instance?
    /// The present target owns its per-frame recorders and drives them
    /// host-side across the begin/end split, so this reads the boxed inner
    /// directly.
    pub(crate) fn host_in_render_pass(&self) -> bool {
        // SAFETY: `self.handle` is `Box::into_raw(Box<RhiCommandRecorderInner>)`;
        // the present target owns the recorder and drives it host-side, so
        // no aliasing `&mut` exists during this shared read.
        unsafe { (*(self.handle as *const RhiCommandRecorderInner)).in_render_pass() }
    }

    /// Abandon an in-progress recording used when a frame is dropped after
    /// `begin()` but before submit (swapchain `OUT_OF_DATE_KHR` on acquire,
    /// or a failed per-frame record). Resets the recorder to `Idle` so the
    /// reused slot begins clean next attempt; harmless when nothing is
    /// recording. See [`RhiCommandRecorderInner::abort_recording`].
    pub fn abort_recording(&mut self) {
        self.host_inner_mut().abort_recording();
    }

    // -------------------------------------------------------------------------
    // Method mirrors. Each forwards to the boxed inner via host_inner_mut().
    // -------------------------------------------------------------------------

    /// Begin a new recording. See [`RhiCommandRecorderInner::begin`].
    pub fn begin(&mut self) -> Result<()> {
        self.host_inner_mut().begin()
    }

    /// Record an image layout transition. See
    /// [`RhiCommandRecorderInner::record_image_barrier`].
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
    pub fn record_buffer_barrier(
        &mut self,
        buffer: &(impl VulkanBufferLike + ?Sized),
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        self.host_inner_mut().record_buffer_barrier(
            buffer,
            from_stage,
            to_stage,
            from_access,
            to_access,
        )
    }

    /// Copy image → buffer. See [`RhiCommandRecorderInner::record_copy_image_to_buffer`].
    pub fn record_copy_image_to_buffer(
        &mut self,
        src: &Texture,
        src_layout: VulkanLayout,
        dst: &(impl VulkanBufferLike + ?Sized),
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_copy_image_to_buffer(src, src_layout, dst, region)
    }

    /// Copy buffer → image.
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

    /// Copy buffer → buffer.
    pub fn record_copy_buffer_to_buffer(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &(impl VulkanBufferLike + ?Sized),
        byte_size: u64,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_copy_buffer_to_buffer(src, dst, byte_size)
    }

    /// Compute dispatch.
    pub fn record_dispatch(
        &mut self,
        kernel: &VulkanComputeKernel,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_dispatch(kernel, group_x, group_y, group_z)
    }

    /// Draw call.
    pub fn record_draw(
        &mut self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawCall,
    ) -> Result<()> {
        self.host_inner_mut().record_draw(kernel, frame_index, draw)
    }

    /// Indexed-draw variant.
    pub fn record_draw_indexed(
        &mut self,
        kernel: &VulkanGraphicsKernel,
        frame_index: u32,
        draw: &DrawIndexedCall,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_draw_indexed(kernel, frame_index, draw)
    }

    /// Submit signaling a timeline semaphore.
    pub fn submit_signaling_timeline(
        &mut self,
        timeline: &HostVulkanTimelineSemaphore,
        signal_value: u64,
    ) -> Result<()> {
        self.host_inner_mut()
            .submit_signaling_timeline(timeline, signal_value)
    }

    /// Submit without semaphore signaling.
    pub fn submit(&mut self) -> Result<()> {
        self.host_inner_mut().submit()
    }

    /// Submit and block until the GPU completes.
    pub fn submit_and_wait(&mut self) -> Result<()> {
        self.host_inner_mut().submit_and_wait()
    }

    /// Block until the most recent submission drains. No-op when nothing is in
    /// flight. Pairs with [`Self::submit`] for a caller that must do something
    /// between handing the queue its work and waiting on it.
    pub fn wait_for_completion(&mut self) -> Result<()> {
        self.host_inner_mut().wait_for_completion()
    }

    /// Engine-internal submit path supporting binary + timeline waits.
    /// **Engine-internal** — for `VulkanPresentTarget`'s render submit.
    pub(crate) fn submit_with_semaphores(
        &mut self,
        waits: &[vk::SemaphoreSubmitInfo],
        signals: &[vk::SemaphoreSubmitInfo],
    ) -> Result<()> {
        self.host_inner_mut().submit_with_semaphores(waits, signals)
    }

    /// Engine-internal swapchain-image layout transition.
    /// **Engine-internal** — for `VulkanPresentTarget`'s pre/post-draw
    /// barriers.
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
        self.host_inner_mut().record_swapchain_image_barrier(
            image, old_layout, new_layout, src_stage, src_access, dst_stage, dst_access,
        )
    }

    /// Engine-internal dynamic-rendering begin.
    /// **Engine-internal** — for
    /// [`PresentFrame::begin_rendering`](super::vulkan_present_target::PresentFrame::begin_rendering).
    pub(crate) fn cmd_begin_dynamic_rendering(
        &mut self,
        image_view: vk::ImageView,
        extent: (u32, u32),
        clear_color: Option<[f32; 4]>,
    ) -> Result<()> {
        self.host_inner_mut()
            .cmd_begin_dynamic_rendering(image_view, extent, clear_color)
    }

    /// Engine-internal dynamic-rendering end.
    /// **Engine-internal** — for
    /// [`PresentFrame::end_rendering`](super::vulkan_present_target::PresentFrame::end_rendering).
    pub(crate) fn cmd_end_dynamic_rendering(&mut self) -> Result<()> {
        self.host_inner_mut().cmd_end_dynamic_rendering()
    }

    // -------------------------------------------------------------------------
    // v3 dispatch helpers (#1066) — match the v1 / v2 helpers above.
    // -------------------------------------------------------------------------
}

impl Drop for RhiCommandRecorder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with `Box::into_raw` in `from_inner`.
            unsafe {
                drop(Box::from_raw(self.handle as *mut RhiCommandRecorderInner));
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

    // ----- Vulkan-free: dynamic-rendering balance producer (#1258) -----
    //
    // The recorder tracks whether its command buffer is inside an open
    // dynamic-rendering instance so `VulkanPresentTarget::end_frame` can
    // close a forgotten pass before the post-draw layout barrier (a
    // `vkCmdPipelineBarrier2` inside an open pass is UB). The device-side
    // `cmd_begin/cmd_end_dynamic_rendering` methods record these exact
    // transitions on `RecorderDynamicRenderingBalance`; testing the pure
    // state type locks the producer half of that seam without a GPU.
    // Mentally reverting `mark_opened()` in `cmd_begin_dynamic_rendering`
    // (leaving the flag clear) makes `end_frame` skip its guard — the
    // consumer half is locked by
    // `vulkan_present_target::tests::end_frame_closes_open_dynamic_rendering_before_present_barrier`.
    #[test]
    fn dynamic_rendering_balance_tracks_open_then_close() {
        let mut balance = RecorderDynamicRenderingBalance::default();
        assert!(
            !balance.is_open(),
            "a fresh recorder has no dynamic-rendering pass open"
        );
        balance.mark_opened(); // cmd_begin_dynamic_rendering records this
        assert!(
            balance.is_open(),
            "after cmd_begin_dynamic_rendering the recorder-inner flag is set"
        );
        balance.mark_closed(); // cmd_end_dynamic_rendering / begin / abort record this
        assert!(
            !balance.is_open(),
            "after cmd_end_dynamic_rendering the recorder-inner flag clears"
        );
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
                entry_point: "main",
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
                entry_point: "main",
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
