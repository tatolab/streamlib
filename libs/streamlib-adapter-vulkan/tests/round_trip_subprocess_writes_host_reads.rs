// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reverse round-trip: subprocess writes via `vkCmdClearColorImage`
//! against the imported VkImage, signals timeline; host waits, reads
//! back, asserts bytes.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

#[test]
fn subprocess_writes_host_reads_round_trip() {
    let host = match common::HostFixture::try_new() {
        Some(h) => h,
        None => {
            println!("round_trip_subprocess_writes_host_reads: skipping — no Vulkan");
            return;
        }
    };

    let surface = host.register_surface(22, 64, 64);

    // Acquire write on the host side first to transition image layout
    // away from UNDEFINED so the subprocess sees a known starting layout.
    {
        let _w = host
            .ctx
            .acquire_write(&surface.descriptor)
            .expect("host warm-up acquire_write");
    }
    assert_eq!(surface.timeline.current_value().unwrap(), 1);

    let (mut child, parent_sock) = common::spawn_helper("write");
    let dma_buf_fd = surface
        .texture
        .vulkan_inner()
        .export_dma_buf_fd()
        .expect("export DMA-BUF");
    let sync_fd = Arc::clone(&surface.timeline)
        .export_opaque_fd()
        .expect("export sync_fd");

    let clear_color = [0.9_f32, 0.1, 0.4, 1.0];
    let req = common::helper_descriptor("write", &surface, 1, Some(clear_color));
    common::send_helper_request(&parent_sock, &req, &[dma_buf_fd], sync_fd)
        .expect("send helper request");

    let resp = common::recv_helper_response(&parent_sock);
    assert_eq!(resp["ok"], true, "helper failed: {}", resp["note"]);
    assert_eq!(child.wait().expect("wait child").code(), Some(0));

    // Helper signaled value 2; host now acquires read and copies bytes
    // back to verify what the subprocess wrote.
    let bytes = host_readback(
        host.adapter.device(),
        surface
            .texture
            .vulkan_inner()
            .image()
            .expect("host image handle"),
        surface.width,
        surface.height,
    );

    // BGRA8 in memory: B=0.4 → 102, G=0.1 → 26, R=0.9 → 230, A=1.0 → 255.
    let mismatch = bytes.chunks_exact(4).enumerate().find(|(_, px)| {
        (px[0] as i32 - 102).abs() > 4
            || (px[1] as i32 - 26).abs() > 4
            || (px[2] as i32 - 230).abs() > 4
            || (px[3] as i32 - 255).abs() > 4
    });
    assert!(
        mismatch.is_none(),
        "host saw wrong pixel after subprocess write: {mismatch:?}"
    );
}

fn host_readback(
    device: &Arc<streamlib::host_rhi::HostVulkanDevice>,
    image: vk::Image,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();
    let bytes = (width as u64) * (height as u64) * 4;

    // Staging buffer.
    let buffer_info = vk::BufferCreateInfo::builder()
        .size(bytes)
        .usage(vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .build();
    let buffer = unsafe { dev.create_buffer(&buffer_info, None) }.expect("create_buffer");
    let mem_req = unsafe { dev.get_buffer_memory_requirements(buffer) };
    let mem_props =
        unsafe { device.instance().get_physical_device_memory_properties(device.physical_device()) };
    let needed =
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mem_type_idx = (0..mem_props.memory_type_count)
        .find(|i| {
            let bit = 1u32 << i;
            (mem_req.memory_type_bits & bit) != 0
                && mem_props.memory_types[*i as usize].property_flags.contains(needed)
        })
        .expect("host-visible memory type");
    let alloc = vk::MemoryAllocateInfo::builder()
        .allocation_size(mem_req.size)
        .memory_type_index(mem_type_idx)
        .build();
    let mem = unsafe { dev.allocate_memory(&alloc, None) }.expect("allocate_memory");
    unsafe { dev.bind_buffer_memory(buffer, mem, 0) }.expect("bind_buffer_memory");

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

    // Subprocess transitioned to TRANSFER_DST_OPTIMAL during clear; we
    // assume image is now in that layout. Transition to TRANSFER_SRC.
    let to_src = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
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
    let bs = [to_src];
    let dep = vk::DependencyInfo::builder().image_memory_barriers(&bs).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let copy = vk::BufferImageCopy::builder()
        .image_subresource(
            vk::ImageSubresourceLayers::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1)
                .build(),
        )
        .image_extent(vk::Extent3D { width, height, depth: 1 })
        .build();
    let regions = [copy];
    unsafe {
        dev.cmd_copy_image_to_buffer(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buffer,
            &regions,
        )
    };

    unsafe { dev.end_command_buffer(cmd) }.expect("end cmd");
    let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build()];
    let submits = [vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build()];
    unsafe { device.submit_to_queue(queue, &submits, vk::Fence::null()) }.expect("submit");
    unsafe { dev.queue_wait_idle(queue) }.expect("queue_wait_idle");

    let mapped = unsafe { dev.map_memory(mem, 0, bytes, vk::MemoryMapFlags::empty()) }
        .expect("map_memory");
    let slice = unsafe { std::slice::from_raw_parts(mapped as *const u8, bytes as usize) };
    let out = slice.to_vec();
    unsafe { dev.unmap_memory(mem) };
    unsafe { dev.destroy_command_pool(pool, None) };
    unsafe { dev.destroy_buffer(buffer, None) };
    unsafe { dev.free_memory(mem, None) };
    out
}
