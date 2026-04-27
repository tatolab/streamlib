// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-process round-trip: host writes a known clear color via the
//! adapter's `acquire_write` scope, releases, subprocess imports the
//! same surface + timeline and reads back. Asserts the bytes the
//! subprocess saw match what the host wrote.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

#[test]
fn host_writes_subprocess_reads_round_trip() {
    let host = match common::HostFixture::try_new() {
        Some(h) => h,
        None => {
            println!("round_trip_host_writes_subprocess_reads: skipping — no Vulkan");
            return;
        }
    };

    let surface = host.register_surface(11, 64, 64);

    // Spawn the subprocess BEFORE the host does GPU work; cross-process
    // dual-VkDevice is fine here per the dual-device crash learning —
    // the subprocess's VkDevice creation runs concurrently with idle host.
    let (mut child, parent_sock) = common::spawn_helper("read");

    // Host write scope: clear the image to a known color via the
    // adapter, release. Adapter advances timeline value 0 → 1.
    {
        let _w = host
            .ctx
            .acquire_write(&surface.descriptor)
            .expect("host acquire_write");
        let cmd_color = [0.25_f32, 0.5, 0.75, 1.0];
        host_clear_image(
            host.adapter.device(),
            surface
                .texture
                .vulkan_inner()
                .image()
                .expect("host image handle"),
            cmd_color,
        );
    }
    assert_eq!(surface.timeline.current_value().unwrap(), 1);

    // Export the host-allocated surface for the subprocess. Timeline
    // sync-fd + DMA-BUF fd both transferred via SCM_RIGHTS.
    let dma_buf_fd = surface
        .texture
        .vulkan_inner()
        .export_dma_buf_fd()
        .expect("export DMA-BUF");
    let sync_fd = Arc::clone(&surface.timeline)
        .export_opaque_fd()
        .expect("export sync_fd");

    let req = common::helper_descriptor("read", &surface, 1, None);
    common::send_helper_request(&parent_sock, &req, &[dma_buf_fd], sync_fd)
        .expect("send helper request");

    let resp = common::recv_helper_response(&parent_sock);
    assert_eq!(resp["ok"], true, "helper failed: {}", resp["note"]);
    assert_eq!(child.wait().expect("wait child").code(), Some(0));

    let bytes = resp["bytes_read"]
        .as_array()
        .expect("bytes_read array")
        .iter()
        .map(|v| v.as_u64().unwrap() as u8)
        .collect::<Vec<_>>();

    // BGRA8 in memory: byte 0=B, 1=G, 2=R, 3=A.
    // ClearColorValue::float32[i] maps to LOGICAL component i (R=0,
    // G=1, B=2, A=3) — independent of memory order. So [0.25, 0.5,
    // 0.75, 1.0] sets R=0.25, G=0.5, B=0.75, A=1.0 → memory bytes
    // [B=191, G=128, R=64, A=255].
    let mismatch = bytes.chunks_exact(4).enumerate().find(|(_, px)| {
        (px[0] as i32 - 191).abs() > 4
            || (px[1] as i32 - 128).abs() > 4
            || (px[2] as i32 - 64).abs() > 4
            || (px[3] as i32 - 255).abs() > 4
    });
    assert!(
        mismatch.is_none(),
        "subprocess read unexpected pixel: {mismatch:?}"
    );

    assert!(surface.timeline.current_value().unwrap() >= 2);
}

fn host_clear_image(
    device: &Arc<streamlib::adapter_support::VulkanDevice>,
    image: vk::Image,
    color: [f32; 4],
) {
    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();

    let pool = unsafe {
        dev.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(qf)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                .build(),
            None,
        )
    }
    .expect("create_command_pool");
    let cmd = unsafe {
        dev.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1)
                .build(),
        )
    }
    .expect("allocate cmd")[0];

    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
    }
    .expect("begin cmd");

    let to_transfer = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let bs = [to_transfer];
    let dep = vk::DependencyInfo::builder().image_memory_barriers(&bs).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let value = vk::ClearColorValue { float32: color };
    let range = vk::ImageSubresourceRange::builder()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
        .build();
    let ranges = [range];
    unsafe {
        dev.cmd_clear_color_image(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &value,
            &ranges,
        )
    };

    let to_general = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let bs2 = [to_general];
    let dep2 = vk::DependencyInfo::builder().image_memory_barriers(&bs2).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep2) };

    unsafe { dev.end_command_buffer(cmd) }.expect("end cmd");
    let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build()];
    let submits = [vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build()];
    unsafe { device.submit_to_queue(queue, &submits, vk::Fence::null()) }.expect("submit");
    unsafe { dev.queue_wait_idle(queue) }.expect("queue_wait_idle");
    unsafe { dev.destroy_command_pool(pool, None) };
}
