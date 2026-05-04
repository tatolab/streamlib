// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Ray-tracing showcase — orbits a single cube at the world origin and
//! renders each frame through `VulkanRayTracingKernel` against a
//! HostVulkanTexture, then writes the frame as a PNG. Intended as the
//! visual gate for `feat(rhi): VulkanRayTracingKernel` (#610) — running
//! it produces a directory of PNGs that can be assembled into an mp4
//! via:
//!
//!     ffmpeg -framerate 30 -i frame_%04d.png \
//!         -c:v libx264 -pix_fmt yuv420p -movflags +faststart \
//!         raytracing_showcase.mp4
//!
//! Skips with a clear message when the device does not expose the
//! `VK_KHR_ray_tracing_pipeline` extension chain.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use streamlib::core::rhi::{
    RayTracingBindingSpec, RayTracingKernelDescriptor, RayTracingPushConstants,
    RayTracingShaderGroup, RayTracingShaderStageFlags, RayTracingStage, StreamTexture,
    TextureDescriptor, TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
    TextureUsages,
};
use streamlib::host_rhi::{
    HostVulkanDevice, HostVulkanTexture, TlasInstanceDesc, VulkanAccelerationStructure,
    VulkanRayTracingKernel, VulkanTextureReadback,
};

const SHOWCASE_RGEN: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/raytracing_showcase.rgen.spv"
));
const SHOWCASE_RMISS: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/raytracing_showcase.rmiss.spv"
));
const SHOWCASE_RCHIT: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/raytracing_showcase.rchit.spv"
));

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAME_COUNT: u32 = 90;

#[repr(C)]
#[derive(Copy, Clone)]
struct PushConstants {
    time: f32,
    aspect: f32,
    width: i32,
    height: i32,
}

fn main() -> Result<()> {
    let out_dir: PathBuf = std::env::var("RT_SHOWCASE_OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("rt-showcase"));
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("create output dir {out_dir:?}"))?;

    println!("Ray-tracing showcase");
    println!("  output dir: {}", out_dir.display());
    println!("  resolution: {WIDTH}x{HEIGHT}");
    println!("  frames:     {FRAME_COUNT}");

    let device = HostVulkanDevice::new().context("create Vulkan device")?;
    if !device.supports_ray_tracing_pipeline() {
        println!(
            "Skipping — device does not expose VK_KHR_ray_tracing_pipeline. \
             Run on an RTX-class or RDNA2+ GPU with a recent driver."
        );
        return Ok(());
    }

    // Build a single-cube scene at the origin. Each frame the rgen shader
    // orbits the camera around it, so a tiny scene is enough to show RT
    // is firing — the camera motion is what makes it readable as video.
    let (cube_vertices, cube_indices) = unit_cube();
    let blas = VulkanAccelerationStructure::build_triangles_blas(
        &device,
        "showcase-cube",
        &cube_vertices,
        &cube_indices,
    )?;

    let tlas_instances = vec![{
        let mut inst = TlasInstanceDesc::identity(Arc::clone(&blas));
        inst.custom_index = 0;
        inst
    }];
    let tlas =
        VulkanAccelerationStructure::build_tlas(&device, "showcase-tlas", &tlas_instances)?;

    let texture = HostVulkanTexture::new_device_local(
        &device,
        &TextureDescriptor {
            label: Some("showcase-output"),
            width: WIDTH,
            height: HEIGHT,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
        },
    )?;
    let stream_texture = StreamTexture::from_vulkan(texture);
    let image = stream_texture
        .vulkan_inner()
        .image()
        .context("texture missing VkImage")?;
    HostVulkanTexture::transition_to_general(&device, image)
        .context("transition output texture to GENERAL")?;

    let stages = [
        RayTracingStage::ray_gen(SHOWCASE_RGEN),
        RayTracingStage::miss(SHOWCASE_RMISS),
        RayTracingStage::closest_hit(SHOWCASE_RCHIT),
    ];
    let groups = [
        RayTracingShaderGroup::General { general: 0 },
        RayTracingShaderGroup::General { general: 1 },
        RayTracingShaderGroup::TrianglesHit {
            closest_hit: Some(2),
            any_hit: None,
        },
    ];
    let bindings = [
        RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    let push = RayTracingPushConstants {
        size: std::mem::size_of::<PushConstants>() as u32,
        stages: RayTracingShaderStageFlags::RAYGEN,
    };
    let kernel = VulkanRayTracingKernel::new(
        &device,
        &RayTracingKernelDescriptor {
            label: "showcase",
            stages: &stages,
            groups: &groups,
            bindings: &bindings,
            push_constants: push,
            max_recursion_depth: 1,
        },
    )?;

    let readback = VulkanTextureReadback::new_into_stream_error(
        &device,
        &TextureReadbackDescriptor {
            label: "showcase-readback",
            format: TextureFormat::Rgba8Unorm,
            width: WIDTH,
            height: HEIGHT,
        },
    )?;

    let aspect = WIDTH as f32 / HEIGHT as f32;
    for frame in 0..FRAME_COUNT {
        let phase = frame as f32 / FRAME_COUNT as f32;
        let push_value = PushConstants {
            time: phase * std::f32::consts::TAU,
            aspect,
            width: WIDTH as i32,
            height: HEIGHT as i32,
        };
        kernel.set_acceleration_structure(0, &tlas)?;
        kernel.set_storage_image(1, &stream_texture)?;
        kernel.set_push_constants_value(&push_value)?;
        kernel.trace_rays(WIDTH, HEIGHT, 1)?;

        let ticket = readback.submit(&stream_texture, TextureSourceLayout::General)?;
        let bytes = readback.wait_and_read(ticket, u64::MAX)?;
        let png_path = out_dir.join(format!("frame_{:04}.png", frame));
        write_rgba_png(&png_path, WIDTH, HEIGHT, bytes)?;
        if frame % 10 == 0 {
            println!("  frame {frame}/{FRAME_COUNT} -> {}", png_path.display());
        }
    }

    println!();
    println!("Wrote {FRAME_COUNT} PNGs to {}", out_dir.display());
    println!("Encode to mp4 with:");
    println!(
        "  ffmpeg -framerate 30 -i {}/frame_%04d.png \\",
        out_dir.display()
    );
    println!("    -c:v libx264 -pix_fmt yuv420p -movflags +faststart \\");
    println!("    {}/raytracing_showcase.mp4", out_dir.display());

    Ok(())
}

fn unit_cube() -> (Vec<f32>, Vec<u32>) {
    // Centered unit cube spanning [-0.5, 0.5]³.
    let vertices: Vec<f32> = vec![
        -0.5, -0.5, -0.5, // 0
         0.5, -0.5, -0.5, // 1
         0.5,  0.5, -0.5, // 2
        -0.5,  0.5, -0.5, // 3
        -0.5, -0.5,  0.5, // 4
         0.5, -0.5,  0.5, // 5
         0.5,  0.5,  0.5, // 6
        -0.5,  0.5,  0.5, // 7
    ];
    let indices: Vec<u32> = vec![
        0, 1, 2, 0, 2, 3, // -Z face
        4, 6, 5, 4, 7, 6, // +Z face
        4, 0, 3, 4, 3, 7, // -X face
        1, 5, 6, 1, 6, 2, // +X face
        0, 4, 5, 0, 5, 1, // -Y face
        3, 2, 6, 3, 6, 7, // +Y face
    ];
    (vertices, indices)
}

// ---- Minimal PNG writer (RGBA8) -------------------------------------------
//
// Mirrors the dependency-free encoder in `display.rs` for the
// PNG-sample path; held inline here so the example doesn't pull a new
// dependency, and so it can be lifted into a tiny standalone helper if a
// future example needs the same shape.

fn write_rgba_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path).with_context(|| format!("create {path:?}"))?;

    file.write_all(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])?;

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6);
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    write_chunk(&mut file, b"IHDR", &ihdr)?;

    let stride = (width as usize) * 4;
    let mut raw = Vec::with_capacity((stride + 1) * (height as usize));
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    let zlib = build_zlib_uncompressed(&raw);
    write_chunk(&mut file, b"IDAT", &zlib)?;
    write_chunk(&mut file, b"IEND", &[])?;
    Ok(())
}

fn write_chunk<W: std::io::Write>(w: &mut W, kind: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(kind)?;
    w.write_all(data)?;
    let crc = crc32(kind, data);
    w.write_all(&crc.to_be_bytes())?;
    Ok(())
}

fn build_zlib_uncompressed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    out.push(0x78);
    out.push(0x01);
    let mut offset = 0;
    while offset < data.len() {
        let chunk_len = (data.len() - offset).min(65535);
        let is_last = offset + chunk_len == data.len();
        out.push(if is_last { 0x01 } else { 0x00 });
        out.extend_from_slice(&(chunk_len as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk_len as u16)).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + chunk_len]);
        offset += chunk_len;
    }
    let adler = adler32(data);
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &b in kind.iter().chain(data.iter()) {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    crc ^ 0xFFFFFFFF
}
