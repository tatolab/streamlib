// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `skia_visual_evidence_gl` — GL-backed companion of
//! `skia_visual_evidence`. Draws the same showcase scene through
//! `SkiaGlSurfaceAdapter` and writes a PNG of the Vulkan-readback so a
//! human can confirm the GL backend is rendering correctly into the
//! shared DMA-BUF.
//!
//! Run with:
//!
//! ```bash
//! STREAMLIB_SKIA_E2E_PNG_DIR=/tmp/skia-evidence-gl \
//!     cargo test -p streamlib-adapter-skia \
//!         --test skia_visual_evidence_gl \
//!         -- --ignored --nocapture
//! ```
//!
//! Same `#[ignore]`-by-default rationale as the Vulkan-backend version.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::Arc;

use skia_safe::{
    gradient_shader, Color, Color4f, Paint, PaintStyle, Path, Point, Rect, TileMode,
};
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib::host_rhi::HostVulkanDevice;
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_opengl::{
    EglRuntime, HostSurfaceRegistration, OpenGlSurfaceAdapter, DRM_FORMAT_ARGB8888,
};
use streamlib_adapter_skia::SkiaGlSurfaceAdapter;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

const W: u32 = 512;
const H: u32 = 512;

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
            println!("skia_visual_evidence_gl: skipping — EGL unavailable: {e}");
            return None;
        }
    };
    Some((gpu, egl))
}

#[test]
#[ignore = "produces an on-disk PNG artifact; run explicitly with --ignored when generating PR evidence"]
fn skia_visual_evidence_gl() {
    let png_dir = match std::env::var("STREAMLIB_SKIA_E2E_PNG_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => panic!(
            "STREAMLIB_SKIA_E2E_PNG_DIR must be set to a directory where the \
             evidence PNG should be written"
        ),
    };
    std::fs::create_dir_all(&png_dir).expect("create_dir_all");
    let png_path = png_dir.join("skia_visual_evidence_gl.png");

    let (gpu, egl) = match try_init() {
        Some(t) => t,
        None => {
            println!("skia_visual_evidence_gl: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let inner = Arc::new(OpenGlSurfaceAdapter::new(Arc::clone(&egl)));
    let skia_gl_adapter = match SkiaGlSurfaceAdapter::new(Arc::clone(&inner)) {
        Ok(a) => a,
        Err(e) => {
            println!("skia_visual_evidence_gl: skipping — Skia GL setup failed: {e}");
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
    let surface_id = 0xe0e1_e0e2u64;
    inner
        .register_host_surface(
            surface_id,
            HostSurfaceRegistration {
                dma_buf_fd,
                width: W,
                height: H,
                drm_fourcc: DRM_FORMAT_ARGB8888,
                drm_format_modifier: modifier,
                plane_offset: plane_layout[0].0,
                plane_stride: plane_layout[0].1,
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

    // === Skia draw: showcase scene (mirrors the Vulkan-backend test) ====
    {
        let mut guard = skia_gl_adapter
            .acquire_write(&surface_desc)
            .expect("skia_gl acquire_write");
        let view = guard.view_mut();
        let canvas = view.surface_mut().canvas();

        // Background: vertical gradient (deep navy → bright cyan)
        let grad = gradient_shader::linear(
            (Point::new(0.0, 0.0), Point::new(0.0, H as f32)),
            &[
                Color4f::new(0.05, 0.10, 0.30, 1.0),
                Color4f::new(0.20, 0.85, 1.00, 1.0),
            ][..],
            None,
            TileMode::Clamp,
            None,
            None,
        )
        .expect("background gradient");
        let mut bg_paint = Paint::default();
        bg_paint.set_shader(grad);
        canvas.draw_rect(Rect::new(0.0, 0.0, W as f32, H as f32), &bg_paint);

        let mut ring = Paint::new(Color4f::new(1.0, 0.85, 0.10, 1.0), None);
        ring.set_style(PaintStyle::Stroke);
        ring.set_stroke_width(8.0);
        ring.set_anti_alias(true);
        canvas.draw_circle(Point::new(W as f32 * 0.5, H as f32 * 0.5), 180.0, &ring);

        let mut disc = Paint::new(Color4f::new(0.95, 0.15, 0.20, 1.0), None);
        disc.set_anti_alias(true);
        canvas.draw_circle(Point::new(W as f32 * 0.5, H as f32 * 0.5), 120.0, &disc);

        let mut lens = Paint::new(Color4f::new(0.85, 0.20, 0.85, 0.55), None);
        lens.set_anti_alias(true);
        canvas.draw_circle(
            Point::new(W as f32 * 0.5 + 60.0, H as f32 * 0.5 - 30.0),
            70.0,
            &lens,
        );

        let mut curve = Path::new();
        let segments = 96;
        let amp: f32 = 38.0;
        let baseline: f32 = H as f32 - 64.0;
        curve.move_to(Point::new(0.0, baseline));
        for i in 1..=segments {
            let t = i as f32 / segments as f32;
            let x = t * W as f32;
            let phase = t * std::f32::consts::PI * 4.0;
            let y = baseline - phase.sin() * amp;
            curve.line_to(Point::new(x, y));
        }
        let mut curve_paint = Paint::new(Color4f::new(1.0, 1.0, 1.0, 0.92), None);
        curve_paint.set_style(PaintStyle::Stroke);
        curve_paint.set_stroke_width(4.0);
        curve_paint.set_anti_alias(true);
        canvas.draw_path(&curve, &curve_paint);

        let strip_y = H as f32 - 30.0;
        let strip_h = 18.0;
        for (i, color) in [
            Color::RED,
            Color::from_argb(255, 255, 165, 0),
            Color::YELLOW,
            Color::GREEN,
            Color::CYAN,
            Color::BLUE,
            Color::from_argb(255, 138, 43, 226),
        ]
        .iter()
        .enumerate()
        {
            let mut tile = Paint::default();
            tile.set_color(*color);
            let x0 = 16.0 + i as f32 * 22.0;
            canvas.draw_rect(
                Rect::new(x0, strip_y, x0 + 18.0, strip_y + strip_h),
                &tile,
            );
        }

        for &(cx, cy) in &[
            (16.0, 16.0),
            (W as f32 - 16.0, 16.0),
            (16.0, H as f32 - 16.0),
            (W as f32 - 16.0, H as f32 - 16.0),
        ] {
            let mut corner = Paint::new(Color4f::new(1.0, 1.0, 1.0, 0.9), None);
            corner.set_anti_alias(true);
            canvas.draw_circle(Point::new(cx, cy), 6.0, &corner);
        }
    } // guard drops → flush_and_submit_surface → glFinish via inner adapter.

    // === Host readback ==============================================
    let pixels = host_readback_bgra(
        &host_device,
        stream_tex.vulkan_inner().image().expect("image handle"),
        W,
        H,
    );
    write_bgra_as_png(&pixels, W, H, &png_path);
    println!(
        "[skia_visual_evidence_gl] wrote {}",
        png_path.display()
    );
}

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

fn write_bgra_as_png(bgra: &[u8], width: u32, height: u32, path: &std::path::Path) {
    use std::fs::File;
    use std::io::BufWriter;

    let mut rgba = bgra.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let file = File::create(path)
        .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .unwrap_or_else(|e| panic!("PNG header: {e}"));
    writer
        .write_image_data(&rgba)
        .unwrap_or_else(|e| panic!("PNG body: {e}"));
}
