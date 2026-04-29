// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `skia_animated_stress` — `#[ignore]`-by-default stress test that
//! drives the Skia adapter through 1800 acquire/draw/flush/release
//! iterations (60 fps × 30 s) with a real animated scene and pipes
//! the host-readback frames into ffmpeg as `bgra` raw video to produce
//! an MP4 plus a hero PNG snapshot.
//!
//! Why this exists:
//!
//! 1. Real-world Skia is animated graphics. The static
//!    `round_trip_skia_canvas` proves a single frame works; this
//!    proves the adapter survives long-running animation without
//!    leaking GPU memory, exhausting descriptor pools, wrapping
//!    timeline counters, or otherwise blowing up.
//! 2. Asserts the timeline counter advances exactly once per frame
//!    (1800 frames → final value 1800). Any per-iteration fence-
//!    pool / submission leak shows up as a counter mismatch or a
//!    timeline wait timeout long before 1800 iterations.
//! 3. Produces a video artifact reviewers can scrub to confirm the
//!    output is visually coherent — color reproduction, AA, alpha
//!    blending, and gradient shaders all stay correct under motion.
//!
//! Run with:
//!
//! ```bash
//! STREAMLIB_SKIA_E2E_VIDEO_DIR=/tmp/skia-stress \
//!     cargo test -p streamlib-adapter-skia \
//!         --test skia_animated_stress \
//!         -- --ignored --nocapture
//! ```
//!
//! Outputs in `$STREAMLIB_SKIA_E2E_VIDEO_DIR`:
//!   - `skia_animated_stress.mp4` — full 30 s × 60 fps H.264 encode.
//!   - `skia_animated_stress_hero.png` — frame at t=15 s.

#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use skia_safe::{
    gradient_shader, Color4f, Paint, PaintStyle, Path, Point, Rect, TileMode,
};
use streamlib::host_rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore};
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_skia::SkiaSurfaceAdapter;
use streamlib_adapter_vulkan::{HostSurfaceRegistration, VulkanLayout, VulkanSurfaceAdapter};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

const W: u32 = 512;
const H: u32 = 512;
const FPS: u32 = 60;
const DURATION_SECS: u32 = 30;
const FRAME_COUNT: u32 = FPS * DURATION_SECS;

fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_skia=warn,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

#[test]
#[ignore = "30 s × 60 fps stress run + ffmpeg encode; explicit-only"]
fn skia_animated_stress() {
    let video_dir = match std::env::var("STREAMLIB_SKIA_E2E_VIDEO_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => panic!(
            "STREAMLIB_SKIA_E2E_VIDEO_DIR must be set to a directory \
             where the MP4 + hero PNG should land"
        ),
    };
    std::fs::create_dir_all(&video_dir).expect("create_dir_all");
    let mp4_path = video_dir.join("skia_animated_stress.mp4");
    let hero_path = video_dir.join("skia_animated_stress_hero.png");

    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("skia_animated_stress: skipping — no Vulkan device");
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let inner = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(&host_device)));
    let skia_adapter = match SkiaSurfaceAdapter::new(Arc::clone(&inner)) {
        Ok(a) => a,
        Err(e) => {
            println!("skia_animated_stress: skipping — Skia setup failed: {e}");
            return;
        }
    };

    // Production allocation path (what the polyglot wrapper will hit).
    let stream_tex = gpu
        .acquire_render_target_dma_buf_image(W, H, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let texture = stream_tex.vulkan_inner().clone();
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(host_device.device(), 0).expect("timeline"),
    );
    let surface_id = 0xa11_a11a;
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

    // Spawn ffmpeg as a child, raw BGRA over stdin → libx264 over MP4
    // out. `-r 60` tags every input frame as a 60 fps frame regardless
    // of the wall-clock generation rate; the file plays back as
    // 30 seconds of 60 fps Skia output.
    // Pin to /usr/bin/ffmpeg explicitly: this machine has a Vulkan-
    // focused ffmpeg 8.0 in /usr/local/bin that lacks libx264.
    // /usr/bin/ffmpeg (apt's 6.x) carries libx264.
    let ffmpeg_bin = if std::path::Path::new("/usr/bin/ffmpeg").exists() {
        "/usr/bin/ffmpeg"
    } else {
        "ffmpeg"
    };
    println!(
        "[skia_animated_stress] spawning {ffmpeg_bin} → {}",
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

    // === Main loop =================================================
    let run_start = Instant::now();
    let mut adapter_times: Vec<Duration> = Vec::with_capacity(FRAME_COUNT as usize);
    let hero_frame_index = (FPS * (DURATION_SECS / 2)) as usize; // ~t=15s
    let mut hero_pixels: Option<Vec<u8>> = None;

    for f in 0..FRAME_COUNT {
        let t = f as f32 / FPS as f32;

        // Adapter scope: acquire → Skia draw → flush + signal on guard
        // drop. Time the whole acquire/draw/flush/release path — that's
        // the canonical "is the adapter capable of 60 fps" metric.
        let adapter_start = Instant::now();
        {
            let mut guard = skia_adapter
                .acquire_write(&surface_desc)
                .expect("skia acquire_write");
            let view = guard.view_mut();
            let canvas = view.surface_mut().canvas();
            draw_animated_frame(canvas, t);
        } // guard drops → flush_and_submit_surface(SyncCpu::Yes) → inner
          // adapter end_write_access → host-signal timeline value f+1.
        let adapter_elapsed = adapter_start.elapsed();
        adapter_times.push(adapter_elapsed);

        // Host readback into a CPU buffer; pipe into ffmpeg.
        // (For real swapchain present this would be a vkQueuePresentKHR
        // instead — readback is the test-rig substitute.)
        let pixels = host_readback_bgra(&host_device, &texture, W, H);
        if (f as usize) == hero_frame_index {
            hero_pixels = Some(pixels.clone());
        }
        ffmpeg_stdin
            .write_all(&pixels)
            .expect("ffmpeg stdin write");

        if (f + 1) % 120 == 0 {
            let wall = run_start.elapsed().as_secs_f32();
            let adapter_avg_ms =
                adapter_times.iter().sum::<Duration>().as_secs_f32() * 1000.0
                    / (f as f32 + 1.0);
            println!(
                "[skia_animated_stress] frame {:>4}/{} wall={:>5.1}s adapter_avg={:>5.2}ms timeline={}",
                f + 1,
                FRAME_COUNT,
                wall,
                adapter_avg_ms,
                timeline.current_value().expect("timeline value"),
            );
        }
    }

    // Close stdin → ffmpeg finalizes the MP4.
    drop(ffmpeg_stdin);
    let ffmpeg_status = ffmpeg.wait().expect("ffmpeg wait");
    assert!(ffmpeg_status.success(), "ffmpeg encode failed: {ffmpeg_status:?}");

    // === Stats ======================================================
    let total = run_start.elapsed();
    let total_s = total.as_secs_f32();
    let adapter_total: Duration = adapter_times.iter().sum();
    let adapter_avg_ms =
        adapter_total.as_secs_f32() * 1000.0 / FRAME_COUNT as f32;
    let adapter_min_ms = adapter_times
        .iter()
        .min()
        .unwrap()
        .as_secs_f32()
        * 1000.0;
    let adapter_max_ms = adapter_times
        .iter()
        .max()
        .unwrap()
        .as_secs_f32()
        * 1000.0;
    let mut sorted = adapter_times.clone();
    sorted.sort_unstable();
    let p50_ms = sorted[FRAME_COUNT as usize / 2].as_secs_f32() * 1000.0;
    let p95_ms = sorted[(FRAME_COUNT as usize * 95) / 100].as_secs_f32() * 1000.0;
    let p99_ms = sorted[(FRAME_COUNT as usize * 99) / 100].as_secs_f32() * 1000.0;

    println!(
        "[skia_animated_stress] DONE — {} frames in {:.2}s wall (incl. readback + ffmpeg I/O)",
        FRAME_COUNT, total_s
    );
    println!(
        "[skia_animated_stress] adapter throughput (acquire/draw/flush/release only): \
         {:.1} fps headroom",
        FRAME_COUNT as f32 / adapter_total.as_secs_f32()
    );
    println!(
        "[skia_animated_stress] adapter frame time: avg={:.2}ms min={:.2}ms p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
        adapter_avg_ms, adapter_min_ms, p50_ms, p95_ms, p99_ms, adapter_max_ms
    );
    println!(
        "[skia_animated_stress] timeline final value = {} (expected {})",
        timeline.current_value().expect("timeline value"),
        FRAME_COUNT
    );

    // === Invariants =================================================
    // The timeline counter must advance exactly once per acquire_write
    // release. Anything else means we lost a signal somewhere.
    assert_eq!(
        timeline.current_value().expect("timeline value"),
        FRAME_COUNT as u64,
        "timeline counter should equal frame count after the run",
    );

    // Adapter must keep up with 60 fps. We give it 4× headroom (16.6ms
    // per frame target → 66ms allowance) so this isn't hardware-flaky.
    let target_avg_ms = 1000.0 / FPS as f32 * 4.0; // 66.6 ms
    assert!(
        adapter_avg_ms < target_avg_ms,
        "adapter average frame time {:.2}ms exceeds 4× the 60 fps budget ({:.2}ms) — \
         likely a leak (descriptor pools / fence pool / GPU memory growing per frame)",
        adapter_avg_ms,
        target_avg_ms,
    );

    // === Hero PNG ===================================================
    if let Some(pixels) = hero_pixels {
        write_bgra_as_png(&pixels, W, H, &hero_path);
        println!(
            "[skia_animated_stress] hero PNG (frame {}, t={:.2}s): {}",
            hero_frame_index,
            hero_frame_index as f32 / FPS as f32,
            hero_path.display()
        );
    }
    println!("[skia_animated_stress] MP4: {}", mp4_path.display());
}

// =============================================================================
// Animated scene — exercises gradients, AA fills, AA strokes, paths,
// alpha blending, and color-cycling under motion.
// =============================================================================

fn draw_animated_frame(canvas: &skia_safe::Canvas, t: f32) {
    use std::f32::consts::TAU;

    // Background — animated linear gradient. Top hue cycles every 9 s,
    // bottom hue offsets by 120°. Tests gradient_shader::linear stability.
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

    // Pulsing rotating ring — exercises stroked path AA + animated transform.
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

    // Five Lissajous-trajectory bouncing balls. Hue-cycling fills,
    // alpha-blending overlap.
    for i in 0..5 {
        let phase = i as f32 * 0.4;
        let fx = 0.5 + 0.6 * (i as f32 * 0.3);
        let fy = 0.7 + 0.5 * (i as f32 * 0.2);
        let x = W as f32 * 0.5 + (t * fx + phase).sin() * (190.0 - i as f32 * 12.0);
        let y = H as f32 * 0.5 + (t * fy + phase).cos() * (180.0 - i as f32 * 14.0);
        let radius = 26.0 + (t * 1.6 + phase).sin() * 10.0;
        let mut color = hsl(t * 70.0 + i as f32 * 72.0, 0.92, 0.58);
        color.a = 0.78; // alpha-blending exercise
        let mut paint = Paint::new(color, None);
        paint.set_anti_alias(true);
        canvas.draw_circle(Point::new(x, y), radius, &paint);
    }

    // 16-spoke starburst — stroked lines, exercises path drawing under
    // rotation. Length pulses with a different period than the ring.
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

    // Traveling sine wave at the bottom — exercises long stroked path
    // with phase animation.
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

    // Hue-cycling color tile strip — confirms color reproduction holds
    // through every frame.
    let strip_y = H as f32 - 22.0;
    let strip_h = 14.0;
    for i in 0..7 {
        let mut tile = Paint::default();
        tile.set_color4f(
            hsl(t * 120.0 + i as f32 * 51.0, 0.95, 0.55),
            None,
        );
        let x0 = 16.0 + i as f32 * 22.0;
        canvas.draw_rect(
            Rect::new(x0, strip_y, x0 + 18.0, strip_y + strip_h),
            &tile,
        );
    }
}

/// Convert HSL to a `Color4f`. h in degrees (any range), s/l in 0..1.
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

// =============================================================================
// Vulkan readback + PNG writer (lifted from `skia_visual_evidence.rs` —
// kept locally so the test file is self-contained).
// =============================================================================

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
