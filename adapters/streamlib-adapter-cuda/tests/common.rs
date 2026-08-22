// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared command recording for the OPAQUE_FD image tests. Pulled in via
//! `#[path = "common.rs"] mod common;` in each test file.

#![cfg(target_os = "linux")]
#![allow(dead_code)] // Each test file uses a different subset.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

/// Record + submit a single-shot command buffer on `queue` and wait
/// on a freshly-allocated fence. Used on both host and consumer sides
/// — these tests' command-buffer needs are simple enough that a single
/// helper covers them.
pub unsafe fn submit_one_shot<F: FnOnce(vk::CommandBuffer)>(
    device: &vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    record: F,
) {
    let pool_info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .build();
    let pool =
        unsafe { device.create_command_pool(&pool_info, None) }.expect("create_command_pool");
    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let cmd = unsafe { device.allocate_command_buffers(&alloc_info) }
        .expect("allocate_command_buffers")[0];

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

/// Record the host-side upload: UNDEFINED → TRANSFER_DST_OPTIMAL, copy
/// `source_buffer` into `image`, then TRANSFER_DST_OPTIMAL →
/// SHADER_READ_ONLY_OPTIMAL — locking the post-upload layout so a future
/// producer wanting to hand off the image with a non-UNDEFINED layout has
/// a reference shape, even though these tests' consumers bridge UNDEFINED →
/// TRANSFER_SRC (see [`record_image_readback_to_buffer`]).
pub unsafe fn record_pattern_upload_to_image(
    device: &vulkanalia::Device,
    cmd: vk::CommandBuffer,
    source_buffer: vk::Buffer,
    image: vk::Image,
    width: u32,
    height: u32,
) {
    let pre_barrier = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::NONE)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
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
    unsafe { device.cmd_pipeline_barrier2(cmd, &pre_dep) };

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
        .image_extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .build();
    let regions = [region];
    let copy_info = vk::CopyBufferToImageInfo2::builder()
        .src_buffer(source_buffer)
        .dst_image(image)
        .dst_image_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .regions(&regions)
        .build();
    unsafe { device.cmd_copy_buffer_to_image2(cmd, &copy_info) };

    let post_barrier = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::COPY)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
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
    unsafe { device.cmd_pipeline_barrier2(cmd, &post_dep) };
}

/// Record the consumer-side readback: bridge UNDEFINED →
/// TRANSFER_SRC_OPTIMAL, then copy `image` into `buffer`.
///
/// The consumer's `VkImage` tracker starts at UNDEFINED by Vulkan spec
/// regardless of the host's post-upload layout (see
/// `docs/learnings/cross-process-vkimage-layout.md`). The bridging
/// transition permits content discard by spec but DMA-BUF / OPAQUE_FD
/// kernel-cache contents are preserved in practice on NVIDIA Linux. The
/// full QFOT acquire path (with `VkExternalMemoryAcquireUnmodifiedEXT`)
/// is the spec-correct content-preserving form when the extension is
/// present; NVIDIA does not ship it as of 2026-05, so the bridge is the
/// structurally permanent path on NVIDIA.
pub unsafe fn record_image_readback_to_buffer(
    device: &vulkanalia::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    buffer: vk::Buffer,
    width: u32,
    height: u32,
) {
    let acquire_barrier = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::NONE)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
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
    unsafe { device.cmd_pipeline_barrier2(cmd, &acquire_dep) };

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
        .image_extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .build();
    let regions = [region];
    let copy_info = vk::CopyImageToBufferInfo2::builder()
        .src_image(image)
        .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .dst_buffer(buffer)
        .regions(&regions)
        .build();
    unsafe { device.cmd_copy_image_to_buffer2(cmd, &copy_info) };
}
