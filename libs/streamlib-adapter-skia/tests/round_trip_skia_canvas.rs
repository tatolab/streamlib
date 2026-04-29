// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `round_trip_skia_canvas` — Skia draws a known shape into a
//! WRITE-acquired surface, then the host reads back the rendered
//! pixels via a HOST_VISIBLE staging buffer + transfer queue copy
//! and asserts the pixel content matches Skia's expected
//! rasterization (with antialiasing tolerance).
//!
//! This is the canonical end-to-end test for the Skia adapter: it
//! exercises every layer — `acquire_write` → wrap as Skia surface →
//! draw via `Canvas` → flush_and_submit → release → host readback.
//! The pixel comparison validates the entire pipeline, including
//! VkImageLayout transitions and timeline-semaphore sync.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use skia_safe::{Color, Color4f, Paint, Point};
use streamlib::host_rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore};
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage,
};
use streamlib_adapter_skia::SkiaSurfaceAdapter;
use streamlib_adapter_vulkan::{HostSurfaceRegistration, VulkanLayout, VulkanSurfaceAdapter};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

const W: u32 = 256;
const H: u32 = 256;

fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_skia=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

#[test]
fn round_trip_skia_canvas() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("round_trip_skia_canvas: skipping — no Vulkan device");
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let inner = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(&host_device)));
    let skia_adapter = match SkiaSurfaceAdapter::new(Arc::clone(&inner)) {
        Ok(a) => a,
        Err(e) => {
            println!("round_trip_skia_canvas: skipping — Skia setup failed: {e}");
            return;
        }
    };

    let stream_tex = gpu
        .acquire_render_target_dma_buf_image(W, H, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let texture = stream_tex.vulkan_inner().clone();
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(host_device.device(), 0).expect("timeline"),
    );
    let surface_id = 0x5e1a_5e1a;
    inner
        .register_host_surface(
            surface_id,
            HostSurfaceRegistration {
                texture: texture.clone(),
                timeline: Arc::clone(&timeline),
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register_host_surface");
    let surface_desc = StreamlibSurface::new(
        surface_id,
        W,
        H,
        SurfaceFormat::Bgra8,
        SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED | SurfaceUsage::CPU_READBACK,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    );

    // === Skia draw scope ============================================
    {
        let mut guard = skia_adapter
            .acquire_write(&surface_desc)
            .expect("skia acquire_write");
        let view = guard.view_mut();
        let canvas = view.surface_mut().canvas();
        // Background — solid blue (BGRA8 with R=0, G=0, B=255, A=255).
        canvas.clear(Color::BLUE);
        // Foreground — bright red disc at the center.
        let mut paint = Paint::new(Color4f::new(1.0, 0.0, 0.0, 1.0), None);
        paint.set_anti_alias(true);
        canvas.draw_circle(Point::new(W as f32 * 0.5, H as f32 * 0.5), 64.0, &paint);
    } // guard drops → flush_and_submit + timeline signal happens here.
    assert!(
        timeline.current_value().expect("timeline value") >= 1,
        "timeline must advance after Skia write",
    );

    // === Host readback ==============================================
    let pixels = host_readback_bgra(&host_device, &texture, W, H);

    // === Pixel-content assertions ===================================
    // Center pixel — inside the red disc — should be roughly pure red.
    let center = sample(&pixels, W as i32 / 2, H as i32 / 2, W);
    assert_pixel_close(
        "center (red disc)",
        center,
        [0, 0, 255, 255], // BGRA8: B=0, G=0, R=255, A=255
        12,
    );
    // Corner pixel — outside the disc — should be roughly pure blue.
    let corner = sample(&pixels, 4, 4, W);
    assert_pixel_close(
        "corner (blue background)",
        corner,
        [255, 0, 0, 255], // BGRA8: B=255, G=0, R=0, A=255
        4,
    );
}

fn sample(pixels: &[u8], x: i32, y: i32, width: u32) -> [u8; 4] {
    let stride = width as usize * 4;
    let off = y as usize * stride + x as usize * 4;
    [
        pixels[off],
        pixels[off + 1],
        pixels[off + 2],
        pixels[off + 3],
    ]
}

fn assert_pixel_close(label: &str, actual: [u8; 4], expected: [u8; 4], tolerance: u8) {
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        let diff = (*a as i32 - *e as i32).unsigned_abs();
        assert!(
            diff <= tolerance as u32,
            "{label}: channel {i} expected ~{e} got {a} (diff {diff}, tolerance {tolerance}), full pixel actual={actual:?} expected={expected:?}"
        );
    }
}

/// Read back BGRA8 pixels from a host-allocated `VkImage` via a
/// HOST_VISIBLE staging buffer + transfer queue copy. The Vulkan
/// adapter's `acquire_write` left the image in `GENERAL` layout; we
/// transition to `TRANSFER_SRC_OPTIMAL` for the copy and back.
fn host_readback_bgra(
    device: &Arc<HostVulkanDevice>,
    texture: &Arc<streamlib::host_rhi::HostVulkanTexture>,
    width: u32,
    height: u32,
) -> Vec<u8> {
    use streamlib::core::rhi::PixelFormat;
    use streamlib::host_rhi::HostVulkanPixelBuffer;

    let staging = HostVulkanPixelBuffer::new(device, width, height, 4, PixelFormat::Bgra32)
        .expect("staging pixel buffer");
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
    let cmds = unsafe {
        dev.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1)
                .build(),
        )
    }
    .expect("allocate_command_buffers");
    let cmd = cmds[0];

    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
    }
    .expect("begin_command_buffer");

    let image = texture.image().expect("texture image");
    let to_transfer = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
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
        .build();
    let barriers = [to_transfer];
    let dep = vk::DependencyInfo::builder()
        .image_memory_barriers(&barriers)
        .build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let region = vk::BufferImageCopy::builder()
        .buffer_offset(0)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(
            vk::ImageSubresourceLayers::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .mip_level(0)
                .base_array_layer(0)
                .layer_count(1)
                .build(),
        )
        .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
        .image_extent(vk::Extent3D { width, height, depth: 1 })
        .build();
    let regions = [region];
    unsafe {
        dev.cmd_copy_image_to_buffer(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            staging.buffer(),
            &regions,
        )
    };

    unsafe { dev.end_command_buffer(cmd) }.expect("end_command_buffer");

    let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build()];
    let submit = vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build();
    unsafe {
        device
            .submit_to_queue(queue, &[submit], vk::Fence::null())
            .expect("submit");
        dev.queue_wait_idle(queue).expect("queue_wait_idle");
        dev.destroy_command_pool(pool, None);
    }

    let size = (width as usize) * (height as usize) * 4;
    let mapped = staging.mapped_ptr();
    assert!(!mapped.is_null());
    let mut out = vec![0u8; size];
    unsafe {
        std::ptr::copy_nonoverlapping(mapped, out.as_mut_ptr(), size);
    }
    out
}
