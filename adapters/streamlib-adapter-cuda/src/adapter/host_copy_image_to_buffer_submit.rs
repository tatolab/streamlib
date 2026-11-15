// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The host-pipeline producer copy: `vkCmdCopyImageToBuffer` submitted with a timeline signal.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use streamlib_consumer_rhi::{
    DevicePrivilege, VulkanLayout, VulkanRhiBuffer, VulkanRhiDevice, VulkanTextureLike,
    VulkanTimelineSemaphoreLike,
};
use streamlib_surface_adapter::{AdapterError, SurfaceId, SurfaceRegistration};
#[cfg(target_os = "linux")]
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::state::SurfaceResource;

use super::{AdapterPersistentSubmitContext, CudaSurfaceAdapter};

/// Snapshot taken under the registry lock so the GPU submit can run
/// unlocked. `write_held` is already set; the rollback path clears it.
/// The producer signals `produce_done` at `signal_value` via the GPU
/// submit's `pSignalSemaphoreInfos` slot per single-writer-per-edge.
struct HostCopySession<P: DevicePrivilege> {
    produce_done: Arc<P::TimelineSemaphore>,
    buffer: vk::Buffer,
    signal_value: u64,
}

impl<D: VulkanRhiDevice> CudaSurfaceAdapter<D> {
    /// Host-pipeline producer path: submit a `vkCmdCopyImageToBuffer`
    /// from `texture` into the surface's registered buffer, with the
    /// timeline GPU-signaled at completion.
    ///
    /// Atomically takes the write lock, computes the next release
    /// value, submits the copy with a timeline signal at that value,
    /// and updates the adapter's release value so subsequent
    /// `acquire_read` calls block on the GPU signal before reading.
    ///
    /// Use case: an in-process producer (camera → cuda copy processor)
    /// pushes frames into a registered cuda surface so a subprocess
    /// AI-inference customer can `acquire_read` them without a CPU
    /// round-trip.
    ///
    /// `texture` is read via [`VulkanTextureLike`] so the caller doesn't
    /// have to import `vulkanalia` — host- and consumer-flavor textures
    /// both implement the trait. `image_layout` is both the layout the
    /// source image is currently in AND the layout it will be returned
    /// to after the copy — the caller's pipeline keeps full ownership
    /// of the layout outside the copy window.
    ///
    /// **Concurrency contract**: the caller must guarantee no other
    /// pipeline stage is operating on `texture` while this method runs.
    /// Camera ring-texture producers satisfy this trivially because each
    /// ring slot is written exactly once per frame and re-used only
    /// after a full ring rotation.
    ///
    /// Errors:
    /// - [`AdapterError::SurfaceNotFound`] — `id` not registered.
    /// - [`AdapterError::WriteContended`] — another writer or reader
    ///   holds the surface. Host writers serialize through this method.
    /// - [`AdapterError::BackendRejected`] — driver refused the submit
    ///   or the texture has no `VkImage`.
    #[cfg(target_os = "linux")]
    pub fn submit_host_copy_image_to_buffer<T>(
        &self,
        id: SurfaceId,
        texture: &T,
        image_layout: VulkanLayout,
    ) -> Result<u64, AdapterError>
    where
        T: VulkanTextureLike + ?Sized,
    {
        let image = texture
            .image()
            .ok_or_else(|| AdapterError::BackendRejected {
                reason:
                    "submit_host_copy_image_to_buffer: source texture has no VkImage (placeholder?)"
                        .into(),
            })?;
        let image_extent = vk::Extent3D {
            width: texture.width(),
            height: texture.height(),
            depth: 1,
        };

        // Conservative dst-bound check: the source texture's pixel
        // count must fit in the destination buffer. We don't know the
        // pixel buffer's bytes-per-pixel from inside the registry
        // closure (the trait doesn't expose it), so we use the buffer
        // size in bytes vs. width*height*4 — Bgra32 is the only
        // OPAQUE_FD format the host allocator emits today
        // (HostVulkanBuffer::new_opaque_fd_export_device_local
        // hardcodes bytes_per_pixel=4 in the example caller). If a
        // future caller uses a non-4-bpp format, extend this check.
        let required_bytes = (image_extent.width as u64)
            .saturating_mul(image_extent.height as u64)
            .saturating_mul(4);

        let session: HostCopySession<D::Privilege> = self
            .surfaces
            .try_begin_write(id, |state| {
                let pixel_buffer = match &state.resource {
                    SurfaceResource::Buffer { pixel_buffer } => pixel_buffer,
                    SurfaceResource::Image { .. } => {
                        return Err(AdapterError::BackendRejected {
                            reason: format!(
                                "submit_host_copy_image_to_buffer: surface_id={id} was \
                                 registered as an image-flavored surface; this host-pipeline \
                                 copy targets a buffer-flavored surface only"
                            ),
                        });
                    }
                };
                let buffer_size = pixel_buffer.size();
                if buffer_size < required_bytes {
                    return Err(AdapterError::BackendRejected {
                        reason: format!(
                            "submit_host_copy_image_to_buffer: source texture is \
                             {}x{}x4 = {} bytes; destination cuda buffer size is \
                             {} bytes (texture would overrun)",
                            image_extent.width, image_extent.height, required_bytes, buffer_size,
                        ),
                    });
                }
                // Claim the signal_value atomically with `write_held`:
                // advance `current_signal_value` here, INSIDE the
                // registry lock, so concurrent submits or
                // `end_*_access` callers in the same process pick
                // strictly greater values. The GPU submit at
                // `signal_value` happens outside the lock; by then
                // state.current is already the about-to-be-signaled
                // value. Under single-writer-per-edge
                // this site is the producer signaling `produce_done`;
                // the consumer's signals run on `consume_done` from a
                // different process counter.
                let signal_value = state.next_signal_value();
                state.current_signal_value = signal_value;
                Ok(HostCopySession {
                    produce_done: Arc::clone(&state.produce_done),
                    buffer: pixel_buffer.buffer(),
                    signal_value,
                })
            })?
            .ok_or_else(|| AdapterError::WriteContended {
                surface_id: id,
                holder: self.surfaces.describe_contention(id),
            })?;

        if let Err(e) = self.submit_image_to_buffer_copy_signal_timeline(
            image,
            image_layout.as_vk(),
            session.buffer,
            image_extent,
            session.produce_done.as_ref(),
            session.signal_value,
        ) {
            // Submit failed; clear write_held. State.current_release_value
            // stays advanced — monotonicity is preserved (the next
            // attempt picks a higher value); only a single value-gap is
            // left in the timeline sequence, which is harmless.
            self.surfaces.rollback_write(id);
            return Err(e);
        }

        // GPU will signal at signal_value asynchronously. Clear the
        // write flag so subsequent `acquire_read` calls can proceed.
        // `with_mut` returns `None` only if the surface raced an
        // unregister between our submit and now — extremely narrow,
        // but log it so the symptom is observable.
        if self
            .surfaces
            .with_mut(id, |state| {
                state.set_write_held(false);
            })
            .is_none()
        {
            tracing::warn!(
                ?id,
                signal_value = session.signal_value,
                "submit_host_copy_image_to_buffer: surface raced unregister after submit"
            );
        }

        Ok(session.signal_value)
    }
}

/// Records and submits the host-pipeline producer copy
/// (`vkCmdCopyImageToBuffer`) using the adapter's persistent command
/// pool ([`AdapterPersistentSubmitContext`]). Transitions `image`
/// into `TRANSFER_SRC_OPTIMAL`, copies its full color plane into
/// `buffer`, transitions back to `image_layout`, and submits with a
/// GPU-side timeline signal at `signal_value`.
///
/// The post-copy buffer barrier is intentionally absent: cuda imports
/// the buffer's memory once at registration and reads it through
/// `cudaWaitExternalSemaphore` on the same timeline, not via host
/// `vkMapMemory`, so a HOST_READ barrier would be both unnecessary
/// and wrong.
#[cfg(target_os = "linux")]
impl<D> CudaSurfaceAdapter<D>
where
    D: VulkanRhiDevice,
{
    pub(super) fn submit_image_to_buffer_copy_signal_timeline(
        &self,
        image: vk::Image,
        image_layout: vk::ImageLayout,
        buffer: vk::Buffer,
        image_extent: vk::Extent3D,
        timeline: &<D::Privilege as DevicePrivilege>::TimelineSemaphore,
        signal_value: u64,
    ) -> Result<(), AdapterError> {
        let vk_device = self.device.device();
        let queue = self.device.queue();
        let qf = self.device.queue_family_index();
        let transfer_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;

        let mut guard = self
            .submit_ctx
            .lock()
            .map_err(|_| AdapterError::BackendRejected {
                reason:
                    "submit_image_to_buffer_copy_signal_timeline: persistent submit context mutex poisoned"
                        .into(),
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

        let pre_barrier = build_color_image_barrier(image, qf, image_layout, transfer_layout);
        let pre_barriers = [pre_barrier];
        let pre_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&pre_barriers)
            .build();
        unsafe { vk_device.cmd_pipeline_barrier2(cmd, &pre_dep) };

        let copy_region = vk::BufferImageCopy::builder()
            .buffer_offset(0)
            .buffer_row_length(image_extent.width)
            .buffer_image_height(image_extent.height)
            .image_subresource(
                vk::ImageSubresourceLayers::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .image_extent(image_extent)
            .build();
        unsafe {
            vk_device.cmd_copy_image_to_buffer(cmd, image, transfer_layout, buffer, &[copy_region]);
        }

        let post_barrier = build_color_image_barrier(image, qf, transfer_layout, image_layout);
        let post_barriers = [post_barrier];
        let post_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&post_barriers)
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
            .semaphore(timeline.semaphore())
            .value(signal_value)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .build()];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .signal_semaphore_infos(&signal_infos)
            .build();

        unsafe {
            self.device
                .submit_to_queue(queue, &[submit], submit_ctx.fence)
        }
        .map_err(|e| AdapterError::BackendRejected {
            reason: format!("submit_to_queue: {e}"),
        })?;

        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn build_color_image_barrier(
    image: vk::Image,
    qf: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
) -> vk::ImageMemoryBarrier2 {
    vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1)
                .build(),
        )
        .build()
}
