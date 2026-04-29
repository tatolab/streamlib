// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `skia_animated_stress_gl` — GL-backed companion of
//! `skia_animated_stress`. Drives `SkiaGlSurfaceAdapter` through 1800
//! acquire/draw/flush/release iterations (60 fps × 30 s) with the same
//! animated scene, pipes host-readback frames to ffmpeg as `bgra` raw
//! video, and produces an MP4 + hero PNG.
//!
//! Why this exists: the Vulkan-backend already has this stress run for
//! a confidence baseline; the GL backend should pass the same gate
//! (no per-frame leaks in the EGL make-current path, no descriptor /
//! fence growth via Skia's GL backend, no glFinish-driven stalls
//! beyond budget) before customers point an existing Skia-on-GL stack
//! at it.
//!
//! Run with:
//!
//! ```bash
//! STREAMLIB_SKIA_E2E_VIDEO_DIR=/tmp/skia-stress-gl \
//!     cargo test -p streamlib-adapter-skia \
//!         --test skia_animated_stress_gl \
//!         -- --ignored --nocapture
//! ```

#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use skia_safe::{
    gradient_shader, Color4f, Paint, PaintStyle, Path, Point, Rect, TileMode,
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
const FPS: u32 = 60;
const DURATION_SECS: u32 = 30;
const FRAME_COUNT: u32 = FPS * DURATION_SECS;

fn try_init() -> Option<(GpuContext, Arc<EglRuntime>)> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(
            "streamlib_adapter_skia=warn,streamlib_adapter_opengl=warn,streamlib=warn",
        )
        .try_init();
    let gpu = GpuContext::init_for_platform_sync().ok()?;
    let egl = match EglRuntime::new() {
        Ok(r) => r,
        Err(e) => {
            println!("skia_animated_stress_gl: skipping — EGL unavailable: {e}");
            return None;
        }
    };
    Some((gpu, egl))
}

#[test]
#[ignore = "30 s × 60 fps stress run + ffmpeg encode; explicit-only"]
fn skia_animated_stress_gl() {
    let video_dir = match std::env::var("STREAMLIB_SKIA_E2E_VIDEO_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => panic!(
            "STREAMLIB_SKIA_E2E_VIDEO_DIR must be set to a directory \
             where the MP4 + hero PNG should land"
        ),
    };
    std::fs::create_dir_all(&video_dir).expect("create_dir_all");
    let mp4_path = video_dir.join("skia_animated_stress_gl.mp4");
    let hero_path = video_dir.join("skia_animated_stress_gl_hero.png");

    let (gpu, egl) = match try_init() {
        Some(t) => t,
        None => {
            println!("skia_animated_stress_gl: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let inner = Arc::new(OpenGlSurfaceAdapter::new(Arc::clone(&egl)));
    let skia_gl_adapter = match SkiaGlSurfaceAdapter::new(Arc::clone(&inner)) {
        Ok(a) => a,
        Err(e) => {
            println!("skia_animated_stress_gl: skipping — Skia GL setup failed: {e}");
            return;
        }
    };

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
    let surface_id = 0xa11_a11bu64;
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

    // Pin to /usr/bin/ffmpeg if present (apt's ffmpeg has libx264);
    // /usr/local/bin's Vulkan-focused build sometimes lacks it.
    let ffmpeg_bin = if std::path::Path::new("/usr/bin/ffmpeg").exists() {
        "/usr/bin/ffmpeg"
    } else {
        "ffmpeg"
    };
    println!(
        "[skia_animated_stress_gl] spawning {ffmpeg_bin} → {}",
        mp4_path.display()
    );
    let mut ffmpeg = Command::new(ffmpeg_bin)
        .args([
            "-y",
            "-loglevel", "error",
            "-f", "rawvideo",
            "-pix_fmt", "bgra",
            "-s", &format!("{}x{}", W, H),
            "-r", &FPS.to_string(),
            "-i", "-",
            "-c:v", "libx264",
            "-pix_fmt", "yuv420p",
            "-preset", "veryfast",
            "-crf", "20",
            "-movflags", "+faststart",
            mp4_path.to_str().expect("mp4 path utf-8"),
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ffmpeg spawn (install ffmpeg if missing)");
    let mut ffmpeg_stdin = ffmpeg.stdin.take().expect("ffmpeg stdin");

    let run_start = Instant::now();
    let mut adapter_times: Vec<Duration> = Vec::with_capacity(FRAME_COUNT as usize);
    let hero_frame_index = (FPS * (DURATION_SECS / 2)) as usize;
    let mut hero_pixels: Option<Vec<u8>> = None;

    for f in 0..FRAME_COUNT {
        let t = f as f32 / FPS as f32;

        let adapter_start = Instant::now();
        {
            let mut guard = skia_gl_adapter
                .acquire_write(&surface_desc)
                .expect("skia_gl acquire_write");
            let view = guard.view_mut();
            let canvas = view.surface_mut().canvas();
            draw_animated_frame(canvas, t);
        } // guard drops → flush_and_submit_surface(SyncCpu::Yes) → glFinish.
        let adapter_elapsed = adapter_start.elapsed();
        adapter_times.push(adapter_elapsed);

        let pixels = host_readback_bgra(
            &host_device,
            stream_tex.vulkan_inner().image().expect("image handle"),
            W,
            H,
        );
        if (f as usize) == hero_frame_index {
            hero_pixels = Some(pixels.clone());
        }
        ffmpeg_stdin.write_all(&pixels).expect("ffmpeg stdin write");

        if (f + 1) % 120 == 0 {
            let wall = run_start.elapsed().as_secs_f32();
            let adapter_avg_ms =
                adapter_times.iter().sum::<Duration>().as_secs_f32() * 1000.0
                    / (f as f32 + 1.0);
            println!(
                "[skia_animated_stress_gl] frame {:>4}/{} wall={:>5.1}s adapter_avg={:>5.2}ms",
                f + 1,
                FRAME_COUNT,
                wall,
                adapter_avg_ms,
            );
        }
    }

    drop(ffmpeg_stdin);
    let ffmpeg_status = ffmpeg.wait().expect("ffmpeg wait");
    assert!(
        ffmpeg_status.success(),
        "ffmpeg encode failed: {ffmpeg_status:?}"
    );

    let total = run_start.elapsed();
    let total_s = total.as_secs_f32();
    let adapter_total: Duration = adapter_times.iter().sum();
    let adapter_avg_ms = adapter_total.as_secs_f32() * 1000.0 / FRAME_COUNT as f32;
    let adapter_min_ms = adapter_times.iter().min().unwrap().as_secs_f32() * 1000.0;
    let adapter_max_ms = adapter_times.iter().max().unwrap().as_secs_f32() * 1000.0;
    let mut sorted = adapter_times.clone();
    sorted.sort_unstable();
    let p50_ms = sorted[FRAME_COUNT as usize / 2].as_secs_f32() * 1000.0;
    let p95_ms = sorted[(FRAME_COUNT as usize * 95) / 100].as_secs_f32() * 1000.0;
    let p99_ms = sorted[(FRAME_COUNT as usize * 99) / 100].as_secs_f32() * 1000.0;

    println!(
        "[skia_animated_stress_gl] DONE — {} frames in {:.2}s wall (incl. readback + ffmpeg I/O)",
        FRAME_COUNT, total_s
    );
    println!(
        "[skia_animated_stress_gl] adapter throughput: {:.1} fps headroom",
        FRAME_COUNT as f32 / adapter_total.as_secs_f32()
    );
    println!(
        "[skia_animated_stress_gl] adapter frame time: avg={:.2}ms min={:.2}ms p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
        adapter_avg_ms, adapter_min_ms, p50_ms, p95_ms, p99_ms, adapter_max_ms
    );

    // 4× the 60 fps budget — sized for hardware/driver headroom, not a
    // tight perf gate. A leak would push avg above this long before
    // 1800 frames complete.
    let target_avg_ms = 1000.0 / FPS as f32 * 4.0;
    assert!(
        adapter_avg_ms < target_avg_ms,
        "adapter average frame time {:.2}ms exceeds 4× the 60 fps budget ({:.2}ms) — \
         likely a leak (descriptor pools / make-current overhead / GPU memory growing per frame)",
        adapter_avg_ms,
        target_avg_ms,
    );

    if let Some(pixels) = hero_pixels {
        write_bgra_as_png(&pixels, W, H, &hero_path);
        println!(
            "[skia_animated_stress_gl] hero PNG (frame {}, t={:.2}s): {}",
            hero_frame_index,
            hero_frame_index as f32 / FPS as f32,
            hero_path.display()
        );
    }
    println!("[skia_animated_stress_gl] MP4: {}", mp4_path.display());
}

fn draw_animated_frame(canvas: &skia_safe::Canvas, t: f32) {
    use std::f32::consts::TAU;

    let top = hsl(t * 40.0, 0.80, 0.20);
    let bottom = hsl(t * 40.0 + 120.0, 0.80, 0.55);
    let bg_grad = gradient_shader::linear(
        (Point::new(0.0, 0.0), Point::new(0.0, H as f32)),
        &[top, bottom][..],
        None,
        TileMode::Clamp,
        None,
        None,
    )
    .expect("background gradient");
    let mut bg = Paint::default();
    bg.set_shader(bg_grad);
    canvas.draw_rect(Rect::new(0.0, 0.0, W as f32, H as f32), &bg);

    let ring_radius = 180.0 + (t * TAU * 0.5).sin() * 18.0;
    let mut ring = Paint::new(hsl(t * 60.0, 0.95, 0.55), None);
    ring.set_style(PaintStyle::Stroke);
    ring.set_stroke_width(6.0 + (t * TAU * 0.3).sin().abs() * 4.0);
    ring.set_anti_alias(true);
    canvas.draw_circle(
        Point::new(W as f32 * 0.5, H as f32 * 0.5),
        ring_radius,
        &ring,
    );

    for i in 0..5 {
        let phase = i as f32 * 0.4;
        let fx = 0.5 + 0.6 * (i as f32 * 0.3);
        let fy = 0.7 + 0.5 * (i as f32 * 0.2);
        let x = W as f32 * 0.5 + (t * fx + phase).sin() * (190.0 - i as f32 * 12.0);
        let y = H as f32 * 0.5 + (t * fy + phase).cos() * (180.0 - i as f32 * 14.0);
        let radius = 26.0 + (t * 1.6 + phase).sin() * 10.0;
        let mut color = hsl(t * 70.0 + i as f32 * 72.0, 0.92, 0.58);
        color.a = 0.78;
        let mut paint = Paint::new(color, None);
        paint.set_anti_alias(true);
        canvas.draw_circle(Point::new(x, y), radius, &paint);
    }

    let spokes = 16;
    let cx = W as f32 * 0.5;
    let cy = H as f32 * 0.5;
    let inner_r = 36.0;
    let outer_r = 70.0 + (t * TAU * 0.4).sin() * 18.0;
    let mut spoke = Paint::new(hsl(t * 90.0 + 200.0, 0.6, 0.85), None);
    spoke.set_style(PaintStyle::Stroke);
    spoke.set_stroke_width(2.0);
    spoke.set_anti_alias(true);
    let mut path = Path::new();
    for s in 0..spokes {
        let a = s as f32 / spokes as f32 * TAU + t * 1.2;
        path.move_to(Point::new(cx + a.cos() * inner_r, cy + a.sin() * inner_r));
        path.line_to(Point::new(cx + a.cos() * outer_r, cy + a.sin() * outer_r));
    }
    canvas.draw_path(&path, &spoke);

    let mut curve = Path::new();
    let segments = 96;
    let amp = 30.0 + (t * 0.7).sin() * 12.0;
    let baseline = H as f32 - 56.0;
    let phase = t * 3.0;
    curve.move_to(Point::new(0.0, baseline));
    for i in 1..=segments {
        let u = i as f32 / segments as f32;
        let x = u * W as f32;
        let y = baseline - (u * TAU * 3.0 + phase).sin() * amp;
        curve.line_to(Point::new(x, y));
    }
    let mut curve_paint = Paint::new(Color4f::new(1.0, 1.0, 1.0, 0.92), None);
    curve_paint.set_style(PaintStyle::Stroke);
    curve_paint.set_stroke_width(3.5);
    curve_paint.set_anti_alias(true);
    canvas.draw_path(&curve, &curve_paint);

    let strip_y = H as f32 - 22.0;
    let strip_h = 14.0;
    for i in 0..7 {
        let mut tile = Paint::default();
        tile.set_color4f(hsl(t * 120.0 + i as f32 * 51.0, 0.95, 0.55), None);
        let x0 = 16.0 + i as f32 * 22.0;
        canvas.draw_rect(
            Rect::new(x0, strip_y, x0 + 18.0, strip_y + strip_h),
            &tile,
        );
    }
}

fn hsl(h: f32, s: f32, l: f32) -> Color4f {
    let h = h.rem_euclid(360.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color4f::new(r + m, g + m, b + m, 1.0)
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
