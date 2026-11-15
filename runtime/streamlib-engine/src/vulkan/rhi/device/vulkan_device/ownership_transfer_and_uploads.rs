// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::Result;
use crate::vulkan::rhi::HostVulkanTexture;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::vulkan::rhi::VulkanTextureLike;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::PixelBufferUploadFinalTextureLayout;
use super::{HostVulkanDevice, host_default_color_subresource_range};

impl HostVulkanDevice {
    /// Copy a host-visible VkBuffer to a device-local VkImage (RGBA upload).
    ///
    /// Transitions the image UNDEFINED → TRANSFER_DST → the destination's
    /// usage-legal terminal layout, which it returns for the caller to
    /// publish.
    ///
    /// Creates and destroys a transient command pool + command buffer + fence
    /// per call — fine for one-shot uses (texture init from a fixture), wrong
    /// for the per-frame hot path. Hot-path callers should use
    /// [`HostVulkanUploadResources`] + [`Self::upload_buffer_to_image_amortized`]
    /// instead.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub unsafe fn upload_buffer_to_image(
        &self,
        src_buffer: vk::Buffer,
        dst_texture: &HostVulkanTexture,
        width: u32,
        height: u32,
    ) -> crate::core::Result<PixelBufferUploadFinalTextureLayout> {
        use crate::core::Error;

        let device = self.device();
        let qf = self.queue_family_index;

        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| Error::GpuError(format!("upload cmd pool: {e}")))?;

        let cb = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| Error::GpuError(format!("upload cmd buf: {e}")))?[0];

        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .map_err(|e| Error::GpuError(format!("upload fence: {e}")))?;

        let final_layout = unsafe {
            self.record_and_submit_buffer_to_image(
                cb,
                fence,
                src_buffer,
                dst_texture,
                width,
                height,
            )
        };

        unsafe { device.destroy_fence(fence, None) };
        unsafe { device.destroy_command_pool(pool, None) };

        final_layout
    }

    /// Amortized hot-path variant of [`Self::upload_buffer_to_image`].
    ///
    /// Reuses caller-provided pool / command buffer / fence — no
    /// `vkCreateCommandPool` / `vkAllocateCommandBuffers` / `vkCreateFence`
    /// per call. The cb is reset (its pool must have
    /// `RESET_COMMAND_BUFFER_BIT`) and the fence is reset before recording.
    /// Submits to the shared queue and blocks on the fence (same sync
    /// model as the per-call variant; only the resource churn is removed).
    ///
    /// Used by [`crate::core::context::TextureRing`]'s per-slot upload
    /// path.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub unsafe fn upload_buffer_to_image_amortized(
        &self,
        cb: vk::CommandBuffer,
        fence: vk::Fence,
        src_buffer: vk::Buffer,
        dst_texture: &HostVulkanTexture,
        width: u32,
        height: u32,
    ) -> crate::core::Result<PixelBufferUploadFinalTextureLayout> {
        use crate::core::Error;

        let device = self.device();
        // Fence may be signaled (from prior submit on this slot) or
        // unsignaled (first use). Reset unconditionally.
        unsafe { device.reset_fences(&[fence]) }
            .map_err(|e| Error::GpuError(format!("amortized upload reset fence: {e}")))?;
        // Cb may be in executable (recorded but not submitted), pending
        // (submitted, before fence wait), or initial state. We just
        // fenced-waited (or first use), so it's safe to reset.
        unsafe { device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()) }
            .map_err(|e| Error::GpuError(format!("amortized upload reset cb: {e}")))?;

        unsafe {
            self.record_and_submit_buffer_to_image(
                cb,
                fence,
                src_buffer,
                dst_texture,
                width,
                height,
            )
        }
    }

    /// Shared by [`Self::upload_buffer_to_image`] and
    /// [`Self::upload_buffer_to_image_amortized`]: records the
    /// UNDEFINED → TRANSFER_DST → copy → usage-legal terminal layout
    /// sequence into `cb`, submits to the shared queue
    /// (mutex-protected), and waits on `fence`. Caller owns cb + fence
    /// lifecycle.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    unsafe fn record_and_submit_buffer_to_image(
        &self,
        cb: vk::CommandBuffer,
        fence: vk::Fence,
        src_buffer: vk::Buffer,
        dst_texture: &HostVulkanTexture,
        width: u32,
        height: u32,
    ) -> crate::core::Result<PixelBufferUploadFinalTextureLayout> {
        use crate::core::Error;

        let device = self.device();
        let queue = self.queue;

        let dst_image = dst_texture.image().ok_or_else(|| {
            Error::GpuError("buffer-to-image upload destination has no VkImage".into())
        })?;
        // Chosen here, at the barrier that records it, rather than by
        // the caller that publishes it.
        let final_texture_layout = dst_texture.terminal_layout_for_shader_read_access();

        unsafe {
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| Error::GpuError(format!("begin cb: {e}")))?;

        // Barrier: UNDEFINED → TRANSFER_DST
        let barrier_to_dst = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let barriers_to_dst = [barrier_to_dst];
        let dep_to_dst = vk::DependencyInfo::builder().image_memory_barriers(&barriers_to_dst);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep_to_dst) };

        // Copy buffer → image
        let region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_offset: vk::Offset3D::default(),
            image_extent: vk::Extent3D {
                width,
                height,
                depth: 1,
            },
        };
        unsafe {
            device.cmd_copy_buffer_to_image(
                cb,
                src_buffer,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            )
        };

        // Barrier: TRANSFER_DST → the destination's usage-legal terminal
        // layout. The destination scope covers every command stage
        // because a GENERAL-terminal image's next consumer is a storage
        // descriptor, a colour attachment or a transfer read, none of
        // which a sampled-read scope would cover.
        let barrier_to_read = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(final_texture_layout.as_vk())
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let barriers_to_read = [barrier_to_read];
        let dep_to_read = vk::DependencyInfo::builder().image_memory_barriers(&barriers_to_read);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep_to_read) };

        unsafe { device.end_command_buffer(cb) }
            .map_err(|e| Error::GpuError(format!("end cb: {e}")))?;

        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe { self.submit_to_queue(queue, &[submit], fence) }?;
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| Error::GpuError(format!("wait: {e}")))?;

        Ok(PixelBufferUploadFinalTextureLayout {
            final_texture_layout,
        })
    }

    /// Whether this device supports content-preserving QFOT for
    /// cross-process layout coordination per #633: the optional
    /// `VK_EXT_external_memory_acquire_unmodified` extension was
    /// enabled at construction.
    ///
    /// `VK_QUEUE_FAMILY_EXTERNAL` (the queue family index used for
    /// QFOT src/dst) is core Vulkan 1.1 and is always available; only
    /// the acquire-unmodified extension is the meaningful gate.
    /// Structurally permanent `false` on NVIDIA Linux per current
    /// driver roadmap (extension not on NVIDIA's published support
    /// list as of 2026-05-03); Mesa is the eventual landing point.
    pub fn supports_qfot_acquire_unmodified(&self) -> bool {
        self.has_acquire_unmodified
    }

    /// Producer-side QFOT release barrier — declares the surface's
    /// post-write `VkImageLayout` and transfers ownership to
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) so a foreign
    /// device (host or peer subprocess) can subsequently acquire it.
    /// Pair with [`Self::acquire_from_foreign`] on the consumer side.
    ///
    /// One-shot synchronous submit on the host graphics queue.
    /// Caller is responsible for publishing the post-release layout
    /// to the surface-share daemon via
    /// `SurfaceStore::update_image_layout`.
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `false`,
    /// falls back to a same-family layout transition; content
    /// preservation across the consumer's import is then driver-
    /// empirical on DMA-BUF.
    pub fn release_to_foreign(
        &self,
        image: vk::Image,
        src_layout: vk::ImageLayout,
        dst_layout: vk::ImageLayout,
    ) -> Result<()> {
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 (promoted from
        // VK_KHR_external_memory) — always available. The Khronos
        // proposal for `VK_EXT_external_memory_acquire_unmodified`
        // permits either EXTERNAL or FOREIGN_EXT as the src/dst; for
        // cross-process Vulkan-to-Vulkan handoff EXTERNAL is the
        // idiomatic choice (FOREIGN_EXT is for non-Vulkan foreign
        // owners, and the OpenGL adapter doesn't issue Vulkan
        // barriers from GL writes anyway).
        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::NONE)
            .dst_access_mask(vk::AccessFlags2::empty())
            .old_layout(src_layout)
            .new_layout(dst_layout)
            .src_queue_family_index(self.queue_family_index)
            .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
            .image(image)
            .subresource_range(host_default_color_subresource_range())
            .build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Consumer-side QFOT acquire barrier — receives ownership from
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) and transitions
    /// the imported `VkImage`'s tracker into `target` so subsequent
    /// host-side consumer barriers (`oldLayout = target → next`) are
    /// validation-clean per VUID-VkImageMemoryBarrier-oldLayout-01197.
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `true`,
    /// chains `VkExternalMemoryAcquireUnmodifiedEXT { acquireUnmodifiedMemory =
    /// VK_TRUE }` so the producer's content survives the transfer per
    /// Vulkan spec. When `false`, falls back to bridging
    /// `UNDEFINED → target` — content discard permitted by spec, but
    /// in practice DMA-BUF kernel-side memory contents are preserved
    /// on every modern Linux Vulkan driver (NVIDIA confirmed; Mesa
    /// iris/radeonsi follow the same convention).
    ///
    /// No-op when `target == UNDEFINED` (nothing to transition into).
    /// Path 2 of
    /// [`crate::core::context::GpuContext::resolve_texture_registration_by_surface_id`]
    /// calls this to align the consumer-side VkImage tracker with the
    /// surface-share IPC's `current_image_layout` (#633).
    pub fn acquire_from_foreign(&self, image: vk::Image, target: vk::ImageLayout) -> Result<()> {
        if target == vk::ImageLayout::UNDEFINED {
            return Ok(());
        }
        // QFOT path requires only `VK_EXT_external_memory_acquire_unmodified`;
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 and always
        // available. When the optional extension is absent (NVIDIA
        // Linux today and for the foreseeable future), fall back to
        // bridging UNDEFINED → target as a same-family transition.
        let use_qfot = self.has_acquire_unmodified;
        let (src_qf, src_layout) = if use_qfot {
            (vk::QUEUE_FAMILY_EXTERNAL, target)
        } else {
            (self.queue_family_index, vk::ImageLayout::UNDEFINED)
        };
        // The chained `VkExternalMemoryAcquireUnmodifiedEXT` lives on
        // the stack here — must outlive the call to
        // `submit_one_shot_image_barriers` because the built barrier
        // holds a raw pointer to it via pNext.
        let mut acquire_unmodified = vk::ExternalMemoryAcquireUnmodifiedEXT::builder()
            .acquire_unmodified_memory(true)
            .build();
        let mut barrier_builder = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(src_layout)
            .new_layout(target)
            .src_queue_family_index(src_qf)
            .dst_queue_family_index(self.queue_family_index)
            .image(image)
            .subresource_range(host_default_color_subresource_range());
        if use_qfot {
            barrier_builder = barrier_builder.push_next(&mut acquire_unmodified);
        }
        let barrier = barrier_builder.build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Internal: one-shot pipeline barrier on the host graphics queue.
    /// Caller passes already-built `vk::ImageMemoryBarrier2` values
    /// (including any chained pNext structs they keep alive on their
    /// own stack); this helper handles command-pool/buffer/fence
    /// creation, submit, wait, and teardown.
    fn submit_one_shot_image_barriers(&self, barriers: &[vk::ImageMemoryBarrier2]) -> Result<()> {
        use crate::core::Error;

        let device = self.device();
        let queue = self.queue;
        let qf = self.queue_family_index;

        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| Error::GpuError(format!("qfot cmd pool: {e}")))?;

        let cb = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| {
            unsafe { device.destroy_command_pool(pool, None) };
            Error::GpuError(format!("qfot cmd buf: {e}"))
        })?[0];

        let fence =
            unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| {
                unsafe { device.destroy_command_pool(pool, None) };
                Error::GpuError(format!("qfot fence: {e}"))
            })?;

        unsafe {
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| Error::GpuError(format!("begin qfot cb: {e}")))?;

        let dep = vk::DependencyInfo::builder().image_memory_barriers(barriers);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep) };

        unsafe { device.end_command_buffer(cb) }
            .map_err(|e| Error::GpuError(format!("end qfot cb: {e}")))?;

        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe { self.submit_to_queue(queue, &[submit], fence) }?;
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| Error::GpuError(format!("qfot wait: {e}")))?;

        unsafe { device.destroy_fence(fence, None) };
        unsafe { device.destroy_command_pool(pool, None) };

        Ok(())
    }
}
