// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `round_trip_skia_canvas_gl` — Skia (GL backend) draws a known shape
//! into a WRITE-acquired surface, then the host reads back the
//! rendered pixels via Vulkan and asserts the pixel content matches
//! Skia's expected rasterization (with antialiasing tolerance).
//!
//! Mirror of `round_trip_skia_canvas` (Vulkan backend), but composed
//! on `OpenGlSurfaceAdapter`. Validates the full Skia-on-GL path:
//! `acquire_write` → wrap as Skia surface → draw via `Canvas` →
//! `flush_and_submit_surface(SyncCpu::Yes)` → `glFinish` → host Vulkan
//! readback through the same DMA-BUF.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use skia_safe::{Color, Color4f, Paint, Point};
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib::host_rhi::HostVulkanDevice;
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage,
};
use streamlib_adapter_opengl::{
    EglRuntime, HostSurfaceRegistration, OpenGlSurfaceAdapter, DRM_FORMAT_ARGB8888,
};
use streamlib_adapter_skia::SkiaGlSurfaceAdapter;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

const W: u32 = 256;
const H: u32 = 256;

fn try_init() -> Option<(GpuContext, Arc<EglRuntime>)> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(
            "streamlib_adapter_skia=debug,streamlib_adapter_opengl=warn,streamlib=warn",
        )
        .try_init();
    let gpu = GpuContext::init_for_platform_sync().ok()?;
    let egl = match EglRuntime::new() {
        Ok(r) => r,
        Err(e) => {
            println!("round_trip_skia_canvas_gl: skipping — EGL unavailable: {e}");
            return None;
        }
    };
    Some((gpu, egl))
}

#[test]
fn round_trip_skia_canvas_gl() {
    let (gpu, egl) = match try_init() {
        Some(t) => t,
        None => {
            println!("round_trip_skia_canvas_gl: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let inner = Arc::new(OpenGlSurfaceAdapter::new(Arc::clone(&egl)));
    let skia_adapter = match SkiaGlSurfaceAdapter::new(Arc::clone(&inner)) {
        Ok(a) => a,
        Err(e) => {
            println!("round_trip_skia_canvas_gl: skipping — Skia GL setup failed: {e}");
            return;
        }
    };

    // === Allocate host DMA-BUF surface, register with the GL adapter ====
    let stream_tex = gpu
        .acquire_render_target_dma_buf_image(W, H, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let dma_buf_fd = stream_tex
        .vulkan_inner()
        .export_dma_buf_fd()
        .expect("export DMA-BUF");
    let plane_layout = stream_tex
        .vulkan_inner()
        .dma_buf_plane_layout()
        .expect("dma_buf_plane_layout");
    let modifier = stream_tex.vulkan_inner().chosen_drm_format_modifier();

    let surface_id = 0xc1ea_5e1au64;
    let registration = HostSurfaceRegistration {
        dma_buf_fd,
        width: W,
        height: H,
        // Vulkan `Bgra8Unorm` is "memory: B,G,R,A". DRM_FORMAT_ARGB8888
        // is the matching fourcc — see `tests/common.rs` of the
        // OpenGL adapter for the full reasoning.
        drm_fourcc: DRM_FORMAT_ARGB8888,
        drm_format_modifier: modifier,
        plane_offset: plane_layout[0].0,
        plane_stride: plane_layout[0].1,
    };
    inner
        .register_host_surface(surface_id, registration)
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

    // === Skia draw scope (GL backend) ===================================
    {
        let mut guard = skia_adapter
            .acquire_write(&surface_desc)
            .expect("skia_gl acquire_write");
        let view = guard.view_mut();
        let canvas = view.surface_mut().canvas();
        // Background — solid blue (BGRA8 with R=0, G=0, B=255, A=255).
        canvas.clear(Color::BLUE);
        // Foreground — bright red disc at the center.
        let mut paint = Paint::new(Color4f::new(1.0, 0.0, 0.0, 1.0), None);
        paint.set_anti_alias(true);
        canvas.draw_circle(Point::new(W as f32 * 0.5, H as f32 * 0.5), 64.0, &paint);
    } // guard drops → flush_and_submit_surface → glFinish.

    // === Host readback ==================================================
    let pixels = host_readback_bgra(&host_device, stream_tex.vulkan_inner().image().expect("image"), W, H);

    // === Pixel-content assertions =======================================
    // Asymmetric tolerances: the corner is far from any geometry and
    // sits inside the flat blue clear, so it must be (255,0,0,255)
    // within driver rounding (tolerance 4). The geometric center sits
    // inside the red disc but near the AA-blended edge transitions of
    // any rasterizer's circle; tolerance 12 accommodates that bleed
    // without weakening the channel-order check (a swap would shift
    // 255 to a different channel and blow either tolerance). Mirrors
    // the Vulkan-backend round-trip test's tolerances.
    let center = sample(&pixels, W as i32 / 2, H as i32 / 2, W);
    assert_pixel_close(
        "center (red disc)",
        center,
        [0, 0, 255, 255], // BGRA8: B=0, G=0, R=255, A=255
        12,
    );
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
/// HOST_VISIBLE staging buffer + transfer queue copy. The OpenGl
/// adapter's `acquire_write` left the image at GENERAL layout (the
/// host-side Vulkan view; GL doesn't change Vulkan's recorded layout
/// across the DMA-BUF handoff). We transition to TRANSFER_SRC_OPTIMAL
/// for the copy and back.
fn host_readback_bgra(
    device: &Arc<HostVulkanDevice>,
    image: vk::Image,
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
                .level_count(1)
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
