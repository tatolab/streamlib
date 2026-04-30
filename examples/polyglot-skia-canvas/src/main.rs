// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot Skia adapter scenario (#577).
//!
//! End-to-end gate for the subprocess `SkiaContext` runtime: the host
//! pre-allocates ONE render-target-capable DMA-BUF surface AND an
//! exportable `HostVulkanTimelineSemaphore`, registers both with
//! surface-share under a known UUID. A Python polyglot processor opens
//! the surface through `SkiaContext.acquire_write` (which under the
//! hood opens `OpenGLContext.acquire_write` to import the DMA-BUF as a
//! `GL_TEXTURE_2D` via EGL, builds a `skia.GrBackendTexture`, and
//! yields a `skia.Surface`), draws a known shape (red disc on blue
//! background), and releases — Skia's flush-and-submit drains the GPU
//! and the inner OpenGL adapter runs `glFinish` so the host's pre-stop
//! readback sees the drawing. This binary then reads the surface back
//! via Vulkan and writes a PNG; reading the PNG with the Read tool is
//! the visual gate.
//!
//! Skia is composed on OpenGL in the subprocess (no `slpn_skia_*`
//! FFI; `streamlib.adapters.skia.SkiaContext` uses the existing
//! `slpn_opengl_*` symbols + skia-python's `GrDirectContext.MakeGL`).
//! The pivot from Vulkan to GL inside Python is forced by skia-python's
//! pybind11 Vulkan binding being unimplemented — see #577 / the
//! adapter's `skia.py` module docstring. The Rust adapter's Vulkan
//! backend is unchanged. Deno is intentionally deferred: there is no
//! maintained Deno Skia binding (same construction-language argument
//! as the abandoned #481 polyglot deferral).
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-skia-canvas/python
//!
//! Run:
//!   cargo run -p polyglot-skia-canvas-scenario -- \
//!       --output=/tmp/skia-canvas-py.png

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::core::rhi::TextureFormat;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::host_rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore};
use streamlib::{BgraFileSourceProcessor, ProcessorSpec, Result, StreamRuntime};

const SCENARIO_SURFACE_UUID: &str = "00000000-0000-0000-0000-000000005c1a";
const SURFACE_SIZE: u32 = 256;

fn main() -> Result<()> {
    let args = std::env::args().skip(1);
    let mut output_png = PathBuf::from("/tmp/skia-canvas-py.png");
    for a in args {
        if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        }
    }

    println!("=== Polyglot Skia adapter canvas scenario (#577) ===");
    println!(
        "Surface:    {SURFACE_SIZE}x{SURFACE_SIZE} BGRA8 (uuid {SCENARIO_SURFACE_UUID})"
    );
    println!("Output PNG: {}", output_png.display());
    println!();

    let runtime = StreamRuntime::new()?;

    let texture_slot: Arc<
        Mutex<Option<streamlib::core::rhi::StreamTexture>>,
    > = Arc::new(Mutex::new(None));
    let device_slot: Arc<Mutex<Option<Arc<HostVulkanDevice>>>> =
        Arc::new(Mutex::new(None));
    let timeline_slot: Arc<Mutex<Option<Arc<HostVulkanTimelineSemaphore>>>> =
        Arc::new(Mutex::new(None));

    {
        let texture_slot = Arc::clone(&texture_slot);
        let device_slot = Arc::clone(&device_slot);
        let timeline_slot = Arc::clone(&timeline_slot);
        runtime.install_setup_hook(move |gpu| {
            // BGRA8: the EGL DMA-BUF importer hands the subprocess a
            // `GL_RGBA8`-typed `GL_TEXTURE_2D` regardless of host
            // channel order; the Python wrapper passes
            // `kBGRA_8888_ColorType` to Skia, which then interprets the
            // bytes back-to-front so what gets drawn ends up in the
            // host's BGRA memory in the right order.
            let texture = gpu.acquire_render_target_dma_buf_image(
                SURFACE_SIZE,
                SURFACE_SIZE,
                TextureFormat::Bgra8Unorm,
            )?;
            let host_device = Arc::clone(gpu.device().vulkan_device());
            let timeline = Arc::new(
                HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
                    .map_err(|e| {
                        StreamError::Configuration(format!(
                            "HostVulkanTimelineSemaphore::new_exportable: {e}"
                        ))
                    })?,
            );
            let store = gpu.surface_store().ok_or_else(|| {
                StreamError::Configuration(
                    "surface_store unavailable — host runtime built without \
                     a surface-share service (Linux subprocess flow requires it)"
                        .into(),
                )
            })?;
            store
                .register_texture(
                    SCENARIO_SURFACE_UUID,
                    &texture,
                    Some(timeline.as_ref()),
                )
                .map_err(|e| {
                    StreamError::Configuration(format!("register_texture: {e}"))
                })?;
            // No bridge wiring: Skia composes on the OpenGL adapter,
            // which has no per-acquire host work — every line of GPU
            // dispatch happens inside the subprocess process via
            // skia-python's GL backend (`MakeGL(MakeEGL())`).
            *texture_slot.lock().unwrap() = Some(texture);
            *device_slot.lock().unwrap() = Some(host_device);
            *timeline_slot.lock().unwrap() = Some(timeline);
            println!(
                "✓ render-target DMA-BUF + timeline registered as '{}'",
                SCENARIO_SURFACE_UUID
            );
            Ok(())
        });
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let slpkg_path =
        manifest_dir.join("python/polyglot-skia-canvas-0.1.0.slpkg");
    if !slpkg_path.exists() {
        return Err(StreamError::Configuration(format!(
            "Package not found: {}\n\
             Run: cargo run -p streamlib-cli -- pack examples/polyglot-skia-canvas/python",
            slpkg_path.display()
        )));
    }
    runtime.load_package(&slpkg_path)?;

    let fixture_path =
        write_trigger_fixture().map_err(StreamError::Configuration)?;
    let source = runtime.add_processor(BgraFileSourceProcessor::Processor::node(
        BgraFileSourceProcessor::Config {
            file_path: fixture_path
                .to_str()
                .ok_or_else(|| {
                    StreamError::Configuration(
                        "fixture path has non-utf8 component".into(),
                    )
                })?
                .to_string(),
            width: 4,
            height: 4,
            fps: 5,
            frame_count: 3,
        },
    ))?;
    println!("+ BgraFileSource: {source}");

    let canvas_config = serde_json::json!({
        "skia_surface_uuid": SCENARIO_SURFACE_UUID,
        "width": SURFACE_SIZE,
        "height": SURFACE_SIZE,
    });
    let canvas = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.skia_canvas",
        canvas_config,
    ))?;
    println!("+ Skia canvas processor: {canvas}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&canvas, "video_in"),
    )?;
    println!("\nPipeline: BgraFileSource → python skia-canvas\n");

    println!("Starting pipeline...");
    runtime.start()?;
    std::thread::sleep(Duration::from_secs(4));
    println!("Stopping pipeline...");
    runtime.stop()?;

    println!("\nReading host surface back via Vulkan...");
    let texture = texture_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| {
            StreamError::Runtime(
                "host texture slot is empty — setup hook never ran".into(),
            )
        })?;
    let device = device_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| StreamError::Runtime("device slot is empty".into()))?;
    let bgra = vulkan_readback(&device, &texture);
    write_png(&bgra, SURFACE_SIZE, SURFACE_SIZE, &output_png)?;
    println!("✓ Output PNG written: {}", output_png.display());
    Ok(())
}

fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;

    let path = std::env::temp_dir().join("skia-canvas-trigger.bgra");
    let mut f = File::create(&path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Read pixels from the host `StreamTexture` back into a CPU buffer.
/// Mirrors the helper in `examples/polyglot-vulkan-compute` —
/// transient HOST_VISIBLE staging buffer + `vkCmdCopyImageToBuffer`
/// + `queue_wait_idle`.
fn vulkan_readback(
    device: &Arc<HostVulkanDevice>,
    texture: &streamlib::core::rhi::StreamTexture,
) -> Vec<u8> {
    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();
    let image = texture
        .vulkan_inner()
        .image()
        .expect("StreamTexture must have a Vulkan image handle on Linux");
    let width = texture.width();
    let height = texture.height();
    let bytes = (width as u64) * (height as u64) * 4;

    let buf = unsafe {
        dev.create_buffer(
            &vk::BufferCreateInfo::builder()
                .size(bytes)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .build(),
            None,
        )
    }
    .expect("create_buffer");
    let mem_req = unsafe { dev.get_buffer_memory_requirements(buf) };
    let inst = device.instance();
    let phys = device.physical_device();
    let mem_props = unsafe { inst.get_physical_device_memory_properties(phys) };
    let needed =
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mem_idx = (0..mem_props.memory_type_count)
        .find(|i| {
            let bit = 1u32 << i;
            (mem_req.memory_type_bits & bit) != 0
                && mem_props.memory_types[*i as usize]
                    .property_flags
                    .contains(needed)
        })
        .expect("host-visible memory type");
    let mem = unsafe {
        dev.allocate_memory(
            &vk::MemoryAllocateInfo::builder()
                .allocation_size(mem_req.size)
                .memory_type_index(mem_idx)
                .build(),
            None,
        )
    }
    .expect("allocate_memory");
    unsafe { dev.bind_buffer_memory(buf, mem, 0) }.expect("bind_buffer_memory");

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
    .expect("allocate_command_buffers")[0];
    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
    }
    .expect("begin_command_buffer");

    // Skia leaves the image in whatever layout it transitioned to
    // during `MakeFromBackendRenderTarget`-driven rendering; the
    // inner Vulkan adapter's release-time `current_layout` isn't
    // updated by Skia. We use a generic `GENERAL` → `TRANSFER_SRC_OPTIMAL`
    // barrier here that's tolerant of either GENERAL or
    // COLOR_ATTACHMENT_OPTIMAL on the way in.
    let to_src = vk::ImageMemoryBarrier2::builder()
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
    let bs = [to_src];
    let dep = vk::DependencyInfo::builder().image_memory_barriers(&bs).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let copy = vk::BufferImageCopy::builder()
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
    let regions = [copy];
    unsafe {
        dev.cmd_copy_image_to_buffer(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buf,
            &regions,
        )
    };

    unsafe { dev.end_command_buffer(cmd) }.expect("end_command_buffer");
    let cmd_infos = [vk::CommandBufferSubmitInfo::builder().command_buffer(cmd).build()];
    let submits = [vk::SubmitInfo2::builder().command_buffer_infos(&cmd_infos).build()];
    unsafe { device.submit_to_queue(queue, &submits, vk::Fence::null()) }
        .expect("submit");
    unsafe { dev.queue_wait_idle(queue) }.expect("queue_wait_idle");

    let mapped = unsafe { dev.map_memory(mem, 0, bytes, vk::MemoryMapFlags::empty()) }
        .expect("map_memory");
    let slice =
        unsafe { std::slice::from_raw_parts(mapped as *const u8, bytes as usize) };
    let out = slice.to_vec();
    unsafe { dev.unmap_memory(mem) };
    unsafe { dev.destroy_command_pool(pool, None) };
    unsafe { dev.destroy_buffer(buf, None) };
    unsafe { dev.free_memory(mem, None) };
    out
}

fn write_png(
    bgra: &[u8],
    width: u32,
    height: u32,
    output: &std::path::Path,
) -> Result<()> {
    use std::fs::File;
    use std::io::BufWriter;

    // Surface is allocated as `Bgra8Unorm`; PNG wants RGBA byte order.
    // Swap the per-pixel B↔R channels.
    let mut rgba = bgra.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }

    let file = File::create(output).map_err(|e| {
        StreamError::Configuration(format!(
            "create output PNG {}: {e}",
            output.display()
        ))
    })?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| StreamError::Configuration(format!("PNG header: {e}")))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| StreamError::Configuration(format!("PNG body: {e}")))?;
    Ok(())
}
