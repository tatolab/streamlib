// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Queue submission, and the one-shot queue family ownership transfers at the acquire and release boundary.

use super::*;

impl ConsumerVulkanDevice {
    /// Submit command buffers under the per-queue mutex.
    ///
    /// # Safety
    /// Standard `vkQueueSubmit2` preconditions apply (valid `submits`,
    /// valid optional `fence`, no concurrent native submits to `queue`
    /// outside this method).
    pub unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        let _lock = self.queue_mutex.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.queue_submit2(queue, submits, fence) }
            .map(|_| ())
            .map_err(|e| ConsumerRhiError::Gpu(format!("queue_submit2 failed: {e}")))
    }

    /// Whether this device supports content-preserving QFOT for
    /// cross-process layout coordination: the optional
    /// `VK_EXT_external_memory_acquire_unmodified` extension was
    /// enabled at construction, letting the consumer-side acquire
    /// barrier chain `VkExternalMemoryAcquireUnmodifiedEXT` so the
    /// producer's contents survive the transfer.
    ///
    /// `VK_QUEUE_FAMILY_EXTERNAL` (the queue family index used for
    /// QFOT src/dst across this carve-out) is core Vulkan 1.1 and is
    /// always available; it is NOT a meaningful capability gate.
    /// Only the acquire-unmodified extension is.
    ///
    /// When `false`, [`Self::release_to_foreign`] and
    /// [`Self::acquire_from_foreign`] fall back to a regular
    /// same-family layout transition; content preservation across
    /// the import boundary then relies on driver-empirical DMA-BUF
    /// kernel semantics rather than the Vulkan spec. To the best of
    /// our current knowledge this fallback is structurally permanent
    /// on NVIDIA Linux (the extension isn't on NVIDIA's roadmap as of
    /// 2026-05-03); on Mesa drivers QFOT is the eventual landing
    /// point.
    pub fn supports_qfot_acquire_unmodified(&self) -> bool {
        self.has_acquire_unmodified
    }

    /// Producer-side QFOT release barrier — declares the surface's
    /// post-write `VkImageLayout` and transfers ownership to
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) so a foreign
    /// device (host or peer subprocess) can subsequently acquire it.
    /// Pair with [`Self::acquire_from_foreign`] on the consumer side.
    ///
    /// One-shot synchronous submit on the device's queue: the
    /// transition is the producer's last GPU work for this surface
    /// before publishing, so blocking on the fence is the simple
    /// correct shape (and matches the cross-process IPC granularity).
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `false`,
    /// falls back to a same-family layout transition — the IPC
    /// `update_image_layout` publish still happens at the call site,
    /// but content preservation across the consumer's import is
    /// driver-empirical.
    pub fn release_to_foreign(
        &self,
        image: vk::Image,
        src_layout: vk::ImageLayout,
        dst_layout: vk::ImageLayout,
    ) -> Result<()> {
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 (promoted from
        // VK_KHR_external_memory) — always available. The Khronos
        // proposal for `VK_EXT_external_memory_acquire_unmodified`
        // explicitly permits either EXTERNAL or FOREIGN_EXT as the
        // src/dst; for cross-process Vulkan-to-Vulkan handoff,
        // EXTERNAL is the idiomatic choice (FOREIGN_EXT is for
        // non-Vulkan foreign owners — OpenGL adapters in this codebase
        // don't issue Vulkan barriers from GL writes anyway).
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
            .subresource_range(default_color_subresource_range())
            .build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Consumer-side QFOT acquire barrier — receives ownership from
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) and transitions
    /// the imported `VkImage`'s tracker into `target` so subsequent
    /// consumer barriers (`oldLayout = target → next`) are
    /// validation-clean per VUID-VkImageMemoryBarrier-oldLayout-01197.
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `true`, chains
    /// `VkExternalMemoryAcquireUnmodifiedEXT { acquireUnmodifiedMemory =
    /// VK_TRUE }` so the producer's content survives the transfer; the
    /// `oldLayout` is `target` (consistent with the post-release layout
    /// the producer published). When `false`, falls back to bridging
    /// `UNDEFINED → target` — content discard permitted by spec, but in
    /// practice DMA-BUF kernel-side memory contents are preserved on
    /// every modern Linux Vulkan driver. No-op when `target ==
    /// UNDEFINED` (nothing to transition into).
    ///
    /// One-shot synchronous submit; the fence wait is the simple correct
    /// shape because the next consumer GPU work assumes the new layout
    /// is visible.
    pub fn acquire_from_foreign(&self, image: vk::Image, target: vk::ImageLayout) -> Result<()> {
        if target == vk::ImageLayout::UNDEFINED {
            return Ok(());
        }
        // QFOT path requires only `VK_EXT_external_memory_acquire_unmodified`;
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 and always
        // available. When the optional extension is absent, fall back
        // to bridging UNDEFINED → target as a same-family transition.
        let use_qfot = self.has_acquire_unmodified;
        let (src_qf, src_layout) = if use_qfot {
            (vk::QUEUE_FAMILY_EXTERNAL, target)
        } else {
            (self.queue_family_index, vk::ImageLayout::UNDEFINED)
        };
        // The chained `VkExternalMemoryAcquireUnmodifiedEXT` lives on the
        // stack here — must outlive the call to `submit_one_shot_image_barriers`,
        // since the built barrier holds a raw pointer to it via pNext.
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
            .subresource_range(default_color_subresource_range());
        if use_qfot {
            barrier_builder = barrier_builder.push_next(&mut acquire_unmodified);
        }
        let barrier = barrier_builder.build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Internal: one-shot pipeline barrier on the device queue. Caller
    /// passes already-built `vk::ImageMemoryBarrier2` values (including
    /// any chained pNext structs they need to keep alive); this helper
    /// handles command-pool/buffer/fence creation, submit, wait, and
    /// teardown.
    fn submit_one_shot_image_barriers(&self, barriers: &[vk::ImageMemoryBarrier2]) -> Result<()> {
        let device = &self.device;

        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(self.queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("qfot cmd pool: {e}")))?;

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
            ConsumerRhiError::Gpu(format!("qfot cmd buf: {e}"))
        })?[0];

        let fence =
            unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| {
                unsafe { device.destroy_command_pool(pool, None) };
                ConsumerRhiError::Gpu(format!("qfot fence: {e}"))
            })?;

        unsafe {
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("begin qfot cb: {e}")))?;

        let dep = vk::DependencyInfo::builder().image_memory_barriers(barriers);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep) };

        unsafe { device.end_command_buffer(cb) }
            .map_err(|e| ConsumerRhiError::Gpu(format!("end qfot cb: {e}")))?;

        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe { self.submit_to_queue(self.queue, &[submit], fence) }?;
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| ConsumerRhiError::Gpu(format!("qfot wait: {e}")))?;

        unsafe { device.destroy_fence(fence, None) };
        unsafe { device.destroy_command_pool(pool, None) };

        Ok(())
    }
}
