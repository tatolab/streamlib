// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The copy trigger for in-process callers holding a host-flavor device.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use streamlib_consumer_rhi::{DevicePrivilege, VulkanRhiDevice, VulkanTimelineSemaphoreLike};
use streamlib_surface_adapter::AdapterError;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use super::adapter_persistent_submit_context::AdapterPersistentSubmitContext;
use super::{
    CopyDirection, CpuReadbackCopyTrigger, CpuReadbackTriggerContext, build_image_barrier,
    combined_aspect_mask, plane_aspect_mask,
};

// =====================================================================
// In-process trigger — generic over `D: VulkanRhiDevice`.
// =====================================================================

/// In-process [`CpuReadbackCopyTrigger`] that records `vkCmdCopy*`
/// against the device and submits via `D::submit_to_queue`. Generic
/// over any `D: VulkanRhiDevice` — works against either flavor of the
/// device, but only meaningful for host-flavor (the consumer's
/// `VkDevice` cannot reach a `VkImage` allocated on the host's
/// device, so the trigger errors when invoked with `image: None`).
///
/// Holds a single persistent `vk::CommandPool` + command buffer +
/// completion fence ([`AdapterPersistentSubmitContext`]), reset and
/// reused on every submit. The pool is lazy-initialised on the first
/// `run_copy_*` call so `new()` stays infallible. Single-threaded
/// caller convention; future concurrent callers serialise through
/// the inner [`Mutex`] (correct, just less concurrent than
/// thread-local pools — see issue #620 AI Agent Notes).
pub struct InProcessCpuReadbackCopyTrigger<D: VulkanRhiDevice> {
    device: Arc<D>,
    submit_ctx: Mutex<Option<AdapterPersistentSubmitContext>>,
    /// Counts the number of times the persistent submit context was
    /// (re)created — incremented on lazy-init and on rebuild after
    /// device loss. Tests assert this stays at 1 after N submits to
    /// lock the amortisation contract from #620.
    submit_ctx_create_count: AtomicUsize,
}

impl<D: VulkanRhiDevice> InProcessCpuReadbackCopyTrigger<D> {
    pub fn new(device: Arc<D>) -> Self {
        Self {
            device,
            submit_ctx: Mutex::new(None),
            submit_ctx_create_count: AtomicUsize::new(0),
        }
    }

    /// Number of times this trigger has materialised its persistent
    /// command pool. Stays at 0 before the first submit, becomes 1
    /// after the first submit, and stays at 1 across every subsequent
    /// submit unless the pool is rebuilt (which never happens today
    /// — driver-loss recovery would bump it).
    ///
    /// Hidden from the public docs because callers shouldn't depend
    /// on it; tests use it to lock #620's amortisation invariant.
    #[doc(hidden)]
    pub fn submit_pool_create_count(&self) -> usize {
        self.submit_ctx_create_count.load(Ordering::Relaxed)
    }
}

impl<D: VulkanRhiDevice + 'static> CpuReadbackCopyTrigger<D::Privilege>
    for InProcessCpuReadbackCopyTrigger<D>
{
    fn run_copy_image_to_buffer(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, D::Privilege>,
    ) -> Result<u64, AdapterError> {
        let image = ctx.image.ok_or(AdapterError::BackendRejected {
            reason:
                "InProcessCpuReadbackCopyTrigger requires a source VkImage; consumer-flavor surfaces have none"
                    .into(),
        })?;
        self.submit_image_buffer_copy(
            image,
            ctx.from_layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            CopyDirection::ImageToBuffer,
            ctx,
        )
    }

    fn run_copy_buffer_to_image(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, D::Privilege>,
    ) -> Result<u64, AdapterError> {
        let image = ctx.image.ok_or(AdapterError::BackendRejected {
            reason: "InProcessCpuReadbackCopyTrigger requires a source VkImage on flush".into(),
        })?;
        self.submit_image_buffer_copy(
            image,
            ctx.from_layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            CopyDirection::BufferToImage,
            ctx,
        )
    }
}

impl<D: VulkanRhiDevice> Drop for InProcessCpuReadbackCopyTrigger<D> {
    fn drop(&mut self) {
        let mut guard = match self.submit_ctx.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(ctx) = guard.take() {
            ctx.destroy(self.device.device());
        }
    }
}

impl<D: VulkanRhiDevice + 'static> InProcessCpuReadbackCopyTrigger<D> {
    fn submit_image_buffer_copy<P: DevicePrivilege>(
        &self,
        image: vk::Image,
        from_layout: vk::ImageLayout,
        transfer_layout: vk::ImageLayout,
        direction: CopyDirection,
        ctx: &CpuReadbackTriggerContext<'_, P>,
    ) -> Result<u64, AdapterError> {
        let vk_device = self.device.device();
        let queue = self.device.queue();
        let qf = self.device.queue_family_index();
        let combined_aspect = combined_aspect_mask(ctx.format);

        let mut guard = self
            .submit_ctx
            .lock()
            .map_err(|_| AdapterError::BackendRejected {
                reason: "submit_image_buffer_copy: persistent submit context mutex poisoned".into(),
            })?;
        if guard.is_none() {
            *guard = Some(AdapterPersistentSubmitContext::new(vk_device, qf)?);
            self.submit_ctx_create_count.fetch_add(1, Ordering::Relaxed);
        }
        let submit_ctx = guard.as_ref().expect("submit_ctx populated above");
        let cmd = submit_ctx.cmd;

        submit_ctx.reset_for_recording(vk_device)?;

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        unsafe { vk_device.begin_command_buffer(cmd, &begin_info) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("begin_command_buffer: {e}"),
            }
        })?;

        let pre_barrier =
            build_image_barrier(image, qf, from_layout, transfer_layout, combined_aspect);
        let pre_barriers = [pre_barrier];
        let pre_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&pre_barriers)
            .build();
        unsafe { vk_device.cmd_pipeline_barrier2(cmd, &pre_dep) };

        for (plane_idx, plane) in ctx.planes.iter().enumerate() {
            let aspect = plane_aspect_mask(ctx.format, plane_idx as u32);
            let copy_region = vk::BufferImageCopy::builder()
                .buffer_offset(0)
                .buffer_row_length(plane.width)
                .buffer_image_height(plane.height)
                .image_subresource(
                    vk::ImageSubresourceLayers::builder()
                        .aspect_mask(aspect)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1)
                        .build(),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width: plane.width,
                    height: plane.height,
                    depth: 1,
                })
                .build();
            match direction {
                CopyDirection::ImageToBuffer => unsafe {
                    vk_device.cmd_copy_image_to_buffer(
                        cmd,
                        image,
                        transfer_layout,
                        plane.buffer,
                        &[copy_region],
                    )
                },
                CopyDirection::BufferToImage => unsafe {
                    vk_device.cmd_copy_buffer_to_image(
                        cmd,
                        plane.buffer,
                        image,
                        transfer_layout,
                        &[copy_region],
                    )
                },
            }
        }

        let post_image_barrier = build_image_barrier(
            image,
            qf,
            transfer_layout,
            vk::ImageLayout::GENERAL,
            combined_aspect,
        );
        let post_image_barriers = [post_image_barrier];
        let post_buf_barriers: Vec<vk::BufferMemoryBarrier2> = match direction {
            CopyDirection::ImageToBuffer => ctx
                .planes
                .iter()
                .map(|p| {
                    vk::BufferMemoryBarrier2::builder()
                        .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                        .dst_access_mask(vk::AccessFlags2::HOST_READ)
                        .buffer(p.buffer)
                        .offset(0)
                        .size(vk::WHOLE_SIZE)
                        .build()
                })
                .collect(),
            CopyDirection::BufferToImage => Vec::new(),
        };
        let post_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&post_image_barriers)
            .buffer_memory_barriers(&post_buf_barriers)
            .build();
        unsafe { vk_device.cmd_pipeline_barrier2(cmd, &post_dep) };

        unsafe { vk_device.end_command_buffer(cmd) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("end_command_buffer: {e}"),
            }
        })?;

        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let signal_infos = [vk::SemaphoreSubmitInfo::builder()
            .semaphore(ctx.produce_done.semaphore())
            .value(ctx.suggested_signal_value)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .build()];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .signal_semaphore_infos(&signal_infos)
            .build();

        submit_ctx
            .submit_carrying_the_fence(|fence| unsafe {
                self.device.submit_to_queue(queue, &[submit], fence)
            })
            .map_err(|e| AdapterError::BackendRejected {
                reason: format!("submit_to_queue: {e}"),
            })?;

        Ok(ctx.suggested_signal_value)
    }
}
