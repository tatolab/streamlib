// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-only round-trip for the OPAQUE_FD `VkImage` carve-out —
//! host → consumer-rhi import → consumer-side
//! `vkCmdCopyImageToBuffer` → byte-equal vs the known host-uploaded
//! pattern.
//!
//! Sibling of [`opaque_fd_consumer_rhi_round_trip.rs`] (which covers
//! OPAQUE_FD `VkBuffer`s); this one covers the image-flavored path
//! the CUDA adapter's tiled-image registration builds on top of.
//!
//! The CUDA-kernel byte-equal assertion through the mapped mipmapped
//! array is **out of scope** here — that requires the CUDA adapter's
//! `cudarc` plumbing, exercised by the adapter-cuda VkImage carve-out
//! test (which depends on these primitives). This test asserts the
//! OPAQUE_FD `VkImage` primitive plumbing only, end-to-end through
//! two separate `VkDevice`s.
//!
//! Test shape:
//!
//! 1. Host allocates an OPAQUE_FD-exportable `VkImage`
//!    ([`HostVulkanTexture::new_opaque_fd_export`]) plus two HOST_VISIBLE
//!    OPAQUE_FD-exportable `VkBuffer`s (source + consumer-staging) via
//!    [`HostVulkanBuffer::new_opaque_fd_export`].
//! 2. Host writes a deterministic pattern through the source buffer's
//!    mapped pointer.
//! 3. Host records and submits `cmd_copy_buffer_to_image` (UNDEFINED →
//!    TRANSFER_DST_OPTIMAL → SHADER_READ_ONLY_OPTIMAL) and waits on a
//!    fence so the upload is complete before the FD export.
//! 4. Host exports OPAQUE_FD FDs for the image and the consumer-staging
//!    buffer.
//! 5. Consumer ([`ConsumerVulkanDevice`]) imports the image via
//!    [`ConsumerVulkanTexture::from_opaque_fd`] and the staging buffer
//!    via [`ConsumerVulkanBuffer::from_opaque_fd`].
//! 6. Consumer records and submits its own `cmd_copy_image_to_buffer`
//!    (UNDEFINED → TRANSFER_SRC_OPTIMAL → copy → done) — `UNDEFINED →
//!    TRANSFER_SRC_OPTIMAL` is the bridging fallback documented in
//!    `docs/learnings/cross-process-vkimage-layout.md`; content
//!    preservation across the bridge is empirical on NVIDIA Linux.
//! 7. Byte-equal assertion: the consumer-side staging buffer's mapped
//!    pointer matches the host's original pattern.
//!
//! Test gating:
//! - `target_os = "linux"` — OPAQUE_FD `VkImage` is Linux-only by
//!   construction.
//! - Skips when Vulkan is unavailable, when the OPAQUE_FD image or
//!   HOST_VISIBLE buffer pool isn't present on the driver, or when the
//!   consumer device can't be created (e.g. UUID mismatch on a
//!   multi-GPU rig).
//! - `#[serial]` — same `VkInstance` / `VkDevice` discipline as the
//!   buffer round-trip carve-out (NVIDIA dual-device crash
//!   `docs/learnings/nvidia-dual-vulkan-device-crash.md`).

#![cfg(target_os = "linux")]

use std::sync::Arc;

use serial_test::serial;
use streamlib::sdk::engine::host_rhi::{
    HostVulkanBuffer, HostVulkanDevice, HostVulkanTexture,
};
use streamlib::sdk::rhi::TextureDescriptor;
use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, ConsumerVulkanDevice, TextureFormat as ConsumerTextureFormat,
    VulkanTextureLike,
};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

const W: u32 = 32;
const H: u32 = 32;
const BYTES_PER_PIXEL: u32 = 4;
const IMAGE_BYTES: u64 = (W as u64) * (H as u64) * (BYTES_PER_PIXEL as u64);

/// Record + submit a single-shot command buffer on `queue` and wait
/// on a freshly-allocated fence. Used on both host and consumer sides
/// — the carve-out test's command-buffer needs are simple enough that
/// a single helper covers them.
unsafe fn submit_one_shot<F: FnOnce(vk::CommandBuffer)>(
    device: &vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    record: F,
) {
    let pool_info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .build();
    let pool = unsafe { device.create_command_pool(&pool_info, None) }
        .expect("create_command_pool");
    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let cmd = unsafe { device.allocate_command_buffers(&alloc_info) }.expect("allocate_command_buffers")[0];

    let begin = vk::CommandBufferBeginInfo::builder()
        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
        .build();
    unsafe { device.begin_command_buffer(cmd, &begin) }.expect("begin_command_buffer");

    record(cmd);

    unsafe { device.end_command_buffer(cmd) }.expect("end_command_buffer");

    let fence_info = vk::FenceCreateInfo::default();
    let fence = unsafe { device.create_fence(&fence_info, None) }.expect("create_fence");

    let cmd_info = vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build();
    let cmd_infos = [cmd_info];
    let submit = vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build();
    let submits = [submit];
    unsafe { device.queue_submit2(queue, &submits, fence) }.expect("queue_submit2");
    unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }.expect("wait_for_fences");

    unsafe {
        device.destroy_fence(fence, None);
        device.destroy_command_pool(pool, None);
    }
}

#[test]
#[serial]
fn opaque_fd_image_carve_out_round_trip() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib=warn,streamlib_consumer_rhi=debug")
        .try_init();

    // Phase 0: skip if Vulkan or the relevant OPAQUE_FD pools aren't
    // available on this driver.
    let host_device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("opaque_fd image carve-out: no Vulkan host device — skipping ({e})");
            return;
        }
    };
    if host_device.opaque_fd_image_pool().is_none() {
        println!(
            "opaque_fd image carve-out: OPAQUE_FD image pool unavailable — driver doesn't \
             support external memory; skipping"
        );
        return;
    }
    if host_device.opaque_fd_buffer_pool().is_none() {
        println!(
            "opaque_fd image carve-out: OPAQUE_FD HOST_VISIBLE buffer pool unavailable — \
             needed for the staging buffers; skipping"
        );
        return;
    }

    // Phase 1: host allocates source staging buffer + image + dest
    // staging buffer (all OPAQUE_FD exportable so the consumer can
    // ride the same import primitive).
    let source_buf = Arc::new(
        HostVulkanBuffer::new_opaque_fd_export(&host_device, IMAGE_BYTES)
            .expect("host source buf new_opaque_fd_export"),
    );
    let dest_buf = Arc::new(
        HostVulkanBuffer::new_opaque_fd_export(&host_device, IMAGE_BYTES)
            .expect("host dest buf new_opaque_fd_export"),
    );
    let desc = TextureDescriptor::new(W, H, streamlib::sdk::rhi::TextureFormat::Rgba8Unorm);
    let host_image = Arc::new(
        HostVulkanTexture::new_opaque_fd_export(&host_device, &desc)
            .expect("host image new_opaque_fd_export"),
    );

    // Phase 2: write a deterministic pattern through the source
    // buffer's mapped pointer.
    let pattern: Vec<u8> = (0..IMAGE_BYTES as usize)
        .map(|i| ((i * 37) & 0xFF) as u8)
        .collect();
    // SAFETY: HOST_VISIBLE | HOST_COHERENT — the mapped pointer is
    // valid for the buffer's lifetime; we hold the Arc.
    unsafe {
        std::ptr::copy_nonoverlapping(
            pattern.as_ptr(),
            source_buf.mapped_ptr(),
            IMAGE_BYTES as usize,
        );
    }

    // Phase 3: host records + submits the upload.
    let host_vk_image = host_image.image().expect("host image vk handle");
    let host_dev = host_device.device();
    let host_queue = host_device.queue();
    let host_qfi = host_device.queue_family_index();

    unsafe {
        submit_one_shot(host_dev, host_queue, host_qfi, |cmd| {
            // UNDEFINED → TRANSFER_DST_OPTIMAL
            let pre_barrier = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(host_vk_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build();
            let pre_barriers = [pre_barrier];
            let pre_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&pre_barriers)
                .build();
            host_dev.cmd_pipeline_barrier2(cmd, &pre_dep);

            // Copy source buffer → image
            let region = vk::BufferImageCopy2::builder()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D { width: W, height: H, depth: 1 })
                .build();
            let regions = [region];
            let copy_info = vk::CopyBufferToImageInfo2::builder()
                .src_buffer(source_buf.buffer())
                .dst_image(host_vk_image)
                .dst_image_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .regions(&regions)
                .build();
            host_dev.cmd_copy_buffer_to_image2(cmd, &copy_info);

            // TRANSFER_DST_OPTIMAL → SHADER_READ_ONLY_OPTIMAL. Locking
            // the post-upload layout so a future producer wanting to
            // hand off the image with a non-UNDEFINED layout has a
            // reference shape, even though this test's consumer
            // bridges UNDEFINED → TRANSFER_SRC (see Phase 6 comment).
            let post_barrier = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(host_vk_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build();
            let post_barriers = [post_barrier];
            let post_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&post_barriers)
                .build();
            host_dev.cmd_pipeline_barrier2(cmd, &post_dep);
        });
    }

    // Phase 4: host exports OPAQUE_FDs for the image and the
    // consumer-side staging buffer.
    let image_fd = host_image
        .export_opaque_fd_memory()
        .expect("export image OPAQUE_FD");
    let dest_fd = dest_buf
        .export_opaque_fd_memory()
        .expect("export dest buf OPAQUE_FD");
    let host_image_alloc_size = host_image.vma_allocation_size();
    let host_dest_size = dest_buf.size() as vk::DeviceSize;
    assert!(
        host_image_alloc_size > 0,
        "host image VMA allocation size must be > 0 for cross-process bind"
    );

    // Phase 5: consumer imports both via consumer-rhi.
    let consumer_device = match ConsumerVulkanDevice::new() {
        Ok(d) => Arc::new(d),
        Err(e) => {
            unsafe {
                libc::close(image_fd);
                libc::close(dest_fd);
            }
            println!(
                "opaque_fd image carve-out: ConsumerVulkanDevice::new failed: {e:?} — skipping \
                 (likely a UUID mismatch on a multi-GPU rig)"
            );
            return;
        }
    };

    let consumer_image = match streamlib_consumer_rhi::ConsumerVulkanTexture::from_opaque_fd(
        &consumer_device,
        image_fd,
        W,
        H,
        ConsumerTextureFormat::Rgba8Unorm,
        host_image_alloc_size,
    ) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            unsafe {
                libc::close(image_fd);
                libc::close(dest_fd);
            }
            panic!("ConsumerVulkanTexture::from_opaque_fd failed: {e}");
        }
    };

    let consumer_dest_buf =
        match ConsumerVulkanBuffer::from_opaque_fd(&consumer_device, dest_fd, host_dest_size) {
            Ok(b) => Arc::new(b),
            Err(e) => {
                // Image fd ownership already transferred to its driver.
                unsafe { libc::close(dest_fd) };
                panic!("ConsumerVulkanBuffer::from_opaque_fd failed: {e}");
            }
        };

    // Phase 5b: assert imported metadata matches.
    assert_eq!(consumer_image.width(), W);
    assert_eq!(consumer_image.height(), H);
    assert_eq!(consumer_image.format(), ConsumerTextureFormat::Rgba8Unorm);
    assert_eq!(consumer_image.vk_image_tiling(), vk::ImageTiling::OPTIMAL);
    // DRM modifier must be zero for OPAQUE_FD imports (no modifier
    // chain on either side).
    assert_eq!(consumer_image.chosen_drm_format_modifier(), 0);

    // Phase 6: consumer-side cmd buffer — bridge UNDEFINED →
    // TRANSFER_SRC_OPTIMAL, copy image to staging buffer.
    //
    // The consumer's `VkImage` tracker starts at UNDEFINED by Vulkan
    // spec regardless of the host's post-upload layout (see
    // `docs/learnings/cross-process-vkimage-layout.md`). The bridging
    // transition permits content discard by spec but DMA-BUF /
    // OPAQUE_FD kernel-cache contents are preserved in practice on
    // NVIDIA Linux. The full QFOT acquire path (with
    // `VkExternalMemoryAcquireUnmodifiedEXT`) is the spec-correct
    // content-preserving form when the extension is present;
    // NVIDIA does not ship it as of 2026-05, so the bridge is the
    // structurally permanent path on NVIDIA.
    let consumer_vk_image = consumer_image.image();
    let consumer_vk_buffer = consumer_dest_buf.buffer();
    let consumer_dev = consumer_device.device();
    let consumer_queue = consumer_device.queue();
    let consumer_qfi = consumer_device.queue_family_index();

    unsafe {
        submit_one_shot(consumer_dev, consumer_queue, consumer_qfi, |cmd| {
            let acquire_barrier = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(consumer_vk_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build();
            let acquire_barriers = [acquire_barrier];
            let acquire_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&acquire_barriers)
                .build();
            consumer_dev.cmd_pipeline_barrier2(cmd, &acquire_dep);

            let region = vk::BufferImageCopy2::builder()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D { width: W, height: H, depth: 1 })
                .build();
            let regions = [region];
            let copy_info = vk::CopyImageToBufferInfo2::builder()
                .src_image(consumer_vk_image)
                .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .dst_buffer(consumer_vk_buffer)
                .regions(&regions)
                .build();
            consumer_dev.cmd_copy_image_to_buffer2(cmd, &copy_info);
        });
    }

    // Phase 7: byte-equal vs the pattern the host originally wrote.
    // SAFETY: HOST_VISIBLE | HOST_COHERENT — the consumer's mapped
    // pointer is valid for the buffer's lifetime; we hold the Arc.
    let consumer_view = unsafe {
        std::slice::from_raw_parts(
            consumer_dest_buf.mapped_ptr(),
            IMAGE_BYTES as usize,
        )
    };
    assert_eq!(
        consumer_view,
        &pattern[..],
        "consumer-side `vkCmdCopyImageToBuffer` on the OPAQUE_FD-imported \
         `VkImage` must produce bytes byte-equal to the host's original \
         upload pattern. Mismatches here indicate either:\n\
         - the cross-process content-preservation invariant broke (driver \
           regression, see docs/learnings/cross-process-vkimage-layout.md), \
         - the host's TRANSFER_DST barrier dropped the upload, or\n\
         - the consumer's bind / FD-import wired memory at the wrong offset \
           or size."
    );
}
