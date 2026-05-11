// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-owned multi-step command-buffer recorder.

use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{Texture, VulkanLayout};
use crate::core::{Error, Result};

use super::{
    HostVulkanDevice, HostVulkanTimelineSemaphore, VulkanAccess, VulkanBufferLike,
    VulkanComputeKernel, VulkanStage,
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
pub struct RhiCommandRecorder {
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

impl RhiCommandRecorder {
    /// Build a recorder against the device's default queue.
    ///
    /// `label` flows into `tracing` spans on every public method —
    /// pick something processor-scoped (e.g. `"camera"`, `"display"`).
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(label))]
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>, label: &str) -> Result<Self> {
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();
        let device = vulkan_device.device().clone();

        let pool_info = vk::CommandPoolCreateInfo::builder()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family_index)
            .build();
        let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
            .map_err(|e| {
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

        let image = texture.inner.image().ok_or_else(|| {
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

        let image = src.inner.image().ok_or_else(|| {
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

        let image = dst.inner.image().ok_or_else(|| {
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

impl Drop for RhiCommandRecorder {
    fn drop(&mut self) {
        unsafe {
            // Wait for any in-flight submission to drain so command-buffer
            // free is safe. Use device_wait_idle as a conservative fence —
            // the completion fence may not have been signaled yet if a
            // recording is in progress without submit.
            let _ = self.device.device_wait_idle();
            self.device.destroy_fence(self.completion_fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

// The recorder holds Vulkan handles that are guarded by its internal
// fence + state machine; the `Mutex<RecorderState>` serializes state
// transitions across threads. `&mut self` on every public method
// further enforces single-thread use.
unsafe impl Send for RhiCommandRecorder {}
unsafe impl Sync for RhiCommandRecorder {}

impl std::fmt::Debug for RhiCommandRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiCommandRecorder")
            .field("label", &self.label)
            .field("state", &*self.state.lock())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{
        ComputeBindingSpec, ComputeKernelDescriptor, PixelBuffer, PixelFormat,
    };
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
        let vk_buf = HostVulkanBuffer::new(device, (element_count as u64) * 4)
            .expect("storage buffer");
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
        assert!(
            msg.contains("outside an active recording"),
            "got: {msg}"
        );
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
        assert!(
            msg.contains("without an active recording"),
            "got: {msg}"
        );
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
        rec.begin().expect("first begin must not block on unsignaled fence");
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

        kernel.set_storage_buffer(0, &input_a).expect("set 0");
        kernel.set_storage_buffer(1, &input_b).expect("set 1");
        kernel.set_storage_buffer(8, &output).expect("set 8");
        let push: [u32; 1] = [element_count];
        kernel
            .set_push_constants_value(&push)
            .expect("push constants");

        let timeline =
            HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline");

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

        let timeline =
            HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline");
        let mut rec =
            RhiCommandRecorder::new(&device, "recorder-back-to-back").expect("recorder");

        for frame in 1..=3u64 {
            kernel.set_storage_buffer(0, &input).expect("set 0");
            kernel.set_storage_buffer(8, &output).expect("set 8");
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
            rec.submit_signaling_timeline(&timeline, frame).expect("submit");

            timeline.wait(frame, u64::MAX).expect("wait");
            assert_eq!(
                timeline.current_value().expect("current_value"),
                frame,
                "timeline counter mismatch after frame {frame}"
            );
        }
    }
}
