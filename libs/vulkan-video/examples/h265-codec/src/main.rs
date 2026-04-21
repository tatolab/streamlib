// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.265 encode/decode example using the BGRA → compute shader → NV12 path.
//!
//! Reads a pre-generated 10-second 1920x1080@60fps BGRA fixture, encodes it
//! through `SimpleEncoder::encode_image()` (which runs the RGB→NV12 compute
//! shader on the GPU), decodes the bitstream back to NV12, and writes the
//! result as a Telegram-compliant MP4 (H.265 video + silent AAC audio).
//!
//! Prerequisites:
//!   ../generate_fixtures.sh    (generates the BGRA fixture files)
//!
//! Usage:
//!   cargo run --release

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use vulkan_video::{
    Codec, Preset, SimpleDecoder, SimpleDecoderConfig, SimpleEncoder, SimpleEncoderConfig,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 60;
const DURATION_SECS: u32 = 10;
const FRAME_COUNT: u32 = FPS * DURATION_SECS;
const BGRA_FRAME_SIZE: usize = (WIDTH * HEIGHT * 4) as usize;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    if let Err(e) = run() {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let example_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = example_dir.join("fixtures");
    let output_dir = example_dir.join("output");
    fs::create_dir_all(&fixture_dir)?;
    fs::create_dir_all(&output_dir)?;

    // --- 1. Verify fixture exists ---
    let fixture_path = fixture_dir.join("smpte_1080p60.bgra");
    let expected_size = BGRA_FRAME_SIZE as u64 * FRAME_COUNT as u64;
    match fs::metadata(&fixture_path) {
        Ok(m) if m.len() == expected_size => {
            println!("Fixture: {} ({} bytes)", fixture_path.display(), m.len());
        }
        Ok(m) => {
            return Err(format!(
                "Fixture size mismatch: expected {} bytes, got {}.\n\
                 Regenerate with: ../generate_fixtures.sh",
                expected_size, m.len()
            ).into());
        }
        Err(_) => {
            return Err(format!(
                "Fixture not found: {}\n\
                 Generate it first with: ../generate_fixtures.sh",
                fixture_path.display()
            ).into());
        }
    }

    // --- 2. Encode ---
    println!("=== H.265 ENCODE (BGRA → compute shader → NV12 → encode) ===");
    let encoder_config = SimpleEncoderConfig {
        width: WIDTH,
        height: HEIGHT,
        fps: FPS,
        codec: Codec::H265,
        preset: Preset::Fast,
        streaming: true,
        idr_interval_secs: 2,
        ..Default::default()
    };

    let mut encoder = SimpleEncoder::new(encoder_config)?;
    let (aligned_w, aligned_h) = encoder.aligned_extent();
    println!(
        "Encoder ready: {}x{} (aligned {}x{})",
        WIDTH, HEIGHT, aligned_w, aligned_h
    );

    // Create a BGRA GPU image + staging buffer on the encoder's device.
    let (bgra_image, bgra_view, bgra_alloc, staging_buf, staging_alloc, staging_ptr) =
        unsafe { create_bgra_upload_resources(&encoder, aligned_w, aligned_h)? };

    let mut bitstream = Vec::new();

    let mut fixture_file = fs::File::open(&fixture_path)?;
    let mut frame_buf = vec![0u8; BGRA_FRAME_SIZE];

    let clock_start = std::time::Instant::now();
    let frame_interval_ns = 1_000_000_000i64 / FPS as i64;

    for frame_idx in 0..FRAME_COUNT {
        fixture_file.read_exact(&mut frame_buf)?;

        // Monotonic timestamp in nanoseconds (matches streamlib's timestamp_ns format).
        let timestamp_ns = clock_start.elapsed().as_nanos() as i64
            + frame_idx as i64 * frame_interval_ns;

        // Upload BGRA frame to GPU and encode.
        let packets = unsafe {
            upload_and_encode(
                &mut encoder,
                &frame_buf,
                bgra_image,
                bgra_view,
                staging_buf,
                staging_ptr,
                aligned_w,
                aligned_h,
                Some(timestamp_ns),
            )?
        };

        for pkt in &packets {
            bitstream.extend_from_slice(&pkt.data);
        }

        if (frame_idx + 1) % 60 == 0 {
            println!(
                "  Encoded {}/{} frames ({:.1}s)",
                frame_idx + 1,
                FRAME_COUNT,
                (frame_idx + 1) as f64 / FPS as f64
            );
        }
    }

    // Flush trailing frames.
    let trailing = encoder.finish()?;
    for pkt in &trailing {
        bitstream.extend_from_slice(&pkt.data);
    }
    println!(
        "  Encode complete: {} bytes bitstream",
        bitstream.len()
    );

    // Clean up BGRA upload resources.
    unsafe {
        let allocator = encoder.allocator();
        encoder.device().device_wait_idle()?;
        encoder.device().destroy_image_view(bgra_view, None);
        allocator.destroy_image(bgra_image, bgra_alloc);
        allocator.destroy_buffer(staging_buf, staging_alloc);
    }

    // --- 3. Decode ---
    println!("\n=== H.265 DECODE ===");
    let decoder_config = SimpleDecoderConfig {
        codec: Codec::H265,
        max_width: WIDTH,
        max_height: HEIGHT,
        ..Default::default()
    };

    let mut decoder = SimpleDecoder::new(decoder_config)?;
    let decoded_frames = decoder.feed(&bitstream)?;
    println!("  Decoded {} frames", decoded_frames.len());

    // --- 4. Mux encoded bitstream into Telegram-compliant MP4 ---
    // Write raw H.265 bitstream to a temp file, then mux directly into MP4
    // with a silent audio track.  No re-encode — preserves exact encoder output.
    let timestamp = chrono_timestamp();
    let output_path = output_dir.join(format!("h265_{timestamp}.mp4"));
    let raw_hevc_path = std::env::temp_dir().join("h265_encoded.hevc");

    {
        let mut hevc_file = fs::File::create(&raw_hevc_path)?;
        hevc_file.write_all(&bitstream)?;
    }

    println!("\n=== CREATING MP4 ===");
    let ffmpeg_status = Command::new("ffmpeg")
        .args([
            "-y",
            "-fflags", "+genpts",
            "-framerate", &FPS.to_string(),
            "-i", raw_hevc_path.to_str().unwrap(),
            "-f", "lavfi",
            "-t", &DURATION_SECS.to_string(),
            "-i", "anullsrc=r=48000:cl=stereo:d=10",
            "-c:v", "copy",
            "-c:a", "aac",
            "-shortest",
            "-movflags", "+faststart",
            output_path.to_str().unwrap(),
        ])
        .status()?;

    if !ffmpeg_status.success() {
        return Err("ffmpeg MP4 mux failed".into());
    }

    let _ = fs::remove_file(&raw_hevc_path);

    println!("\n=== DONE ===");
    println!("Output: {}", output_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Vulkan BGRA image upload helpers
// ---------------------------------------------------------------------------

/// Create a BGRA GPU image, image view, and staging buffer on the encoder's
/// device for uploading BGRA frames to pass to `encode_image()`.
unsafe fn create_bgra_upload_resources(
    encoder: &SimpleEncoder,
    aligned_w: u32,
    aligned_h: u32,
) -> Result<
    (vk::Image, vk::ImageView, vma::Allocation, vk::Buffer, vma::Allocation, *mut u8),
    Box<dyn std::error::Error>,
> {
    let device = encoder.device();
    let allocator = encoder.allocator();
    let (transfer_qf, _) = encoder.transfer_queue();
    let (compute_qf, _) = encoder.compute_queue();

    // The BGRA image needs SAMPLED (for compute shader texelFetch) and
    // TRANSFER_DST (for staging upload).  Use CONCURRENT sharing between
    // transfer and compute queue families.
    let queue_families = [transfer_qf, compute_qf];
    let mut image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width: aligned_w,
            height: aligned_h,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
        .initial_layout(vk::ImageLayout::UNDEFINED);

    if transfer_qf != compute_qf {
        image_info = image_info
            .sharing_mode(vk::SharingMode::CONCURRENT)
            .queue_family_indices(&queue_families);
    } else {
        image_info = image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
    }

    let alloc_options = vma::AllocationOptions {
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };
    let (image, allocation) = unsafe { allocator.create_image(image_info, &alloc_options) }?;

    let view = unsafe {
        device.create_image_view(
            &vk::ImageViewCreateInfo::builder()
                .image(image)
                .view_type(vk::ImageViewType::_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                }),
            None,
        )
    }?;

    // Staging buffer (host-visible, for CPU → GPU upload).
    let staging_size = (aligned_w * aligned_h * 4) as u64;
    let staging_info = vk::BufferCreateInfo::builder()
        .size(staging_size)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC);

    let staging_opts = vma::AllocationOptions {
        required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        flags: vma::AllocationCreateFlags::MAPPED
            | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
        ..Default::default()
    };
    let (staging_buf, staging_alloc) = unsafe { allocator.create_buffer(staging_info, &staging_opts) }?;
    let info = allocator.get_allocation_info(staging_alloc);
    let staging_ptr = info.pMappedData as *mut u8;

    Ok((image, view, allocation, staging_buf, staging_alloc, staging_ptr))
}

/// Upload a BGRA frame to the staging buffer, copy to the GPU image,
/// transition to SHADER_READ_ONLY_OPTIMAL, and call `encode_image()`.
unsafe fn upload_and_encode(
    encoder: &mut SimpleEncoder,
    bgra_data: &[u8],
    bgra_image: vk::Image,
    bgra_view: vk::ImageView,
    staging_buf: vk::Buffer,
    staging_ptr: *mut u8,
    aligned_w: u32,
    aligned_h: u32,
    timestamp_ns: Option<i64>,
) -> Result<Vec<vulkan_video::EncodePacket>, Box<dyn std::error::Error>> {
    let device = encoder.device().clone();
    let (transfer_qf, transfer_queue) = encoder.transfer_queue();

    // Copy BGRA pixels into staging buffer.
    // If the frame is smaller than the aligned extent, copy row by row.
    let src_row_bytes = (WIDTH * 4) as usize;
    let dst_row_bytes = (aligned_w * 4) as usize;
    if src_row_bytes == dst_row_bytes && WIDTH == aligned_w && HEIGHT == aligned_h {
        unsafe { std::ptr::copy_nonoverlapping(bgra_data.as_ptr(), staging_ptr, bgra_data.len()) };
    } else {
        // Clear the staging buffer first (padding rows/columns).
        unsafe { std::ptr::write_bytes(staging_ptr, 0, (aligned_w * aligned_h * 4) as usize) };
        for row in 0..HEIGHT as usize {
            let src_off = row * src_row_bytes;
            let dst_off = row * dst_row_bytes;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bgra_data.as_ptr().add(src_off),
                    staging_ptr.add(dst_off),
                    src_row_bytes,
                )
            };
        }
    }

    // Record transfer commands: copy staging → GPU image, then transition.
    // We create a one-shot command pool per call for simplicity.
    let pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(transfer_qf)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT),
            None,
        )
    }?;
    let cb = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )
    }?[0];

    unsafe {
        device.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
    }?;

    // Transition BGRA image: UNDEFINED → TRANSFER_DST_OPTIMAL
    let barrier_to_dst = vk::ImageMemoryBarrier::builder()
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(bgra_image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);

    unsafe {
        device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[] as &[vk::MemoryBarrier],
            &[] as &[vk::BufferMemoryBarrier],
            &[barrier_to_dst],
        )
    };

    // Copy staging buffer → BGRA image.
    let region = vk::BufferImageCopy::builder()
        .buffer_offset(0)
        .buffer_row_length(aligned_w)
        .buffer_image_height(aligned_h)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: aligned_w,
            height: aligned_h,
            depth: 1,
        });

    unsafe {
        device.cmd_copy_buffer_to_image(
            cb,
            staging_buf,
            bgra_image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        )
    };

    // Transition BGRA image: TRANSFER_DST_OPTIMAL → SHADER_READ_ONLY_OPTIMAL
    let barrier_to_read = vk::ImageMemoryBarrier::builder()
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(bgra_image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::SHADER_READ);

    unsafe {
        device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &[] as &[vk::MemoryBarrier],
            &[] as &[vk::BufferMemoryBarrier],
            &[barrier_to_read],
        )
    };

    unsafe { device.end_command_buffer(cb) }?;

    // Submit and wait.
    let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }?;
    unsafe {
        device.queue_submit(
            transfer_queue,
            &[vk::SubmitInfo::builder().command_buffers(&[cb])],
            fence,
        )
    }?;
    unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }?;
    unsafe { device.destroy_fence(fence, None) };
    unsafe { device.destroy_command_pool(pool, None) };

    // Now the BGRA image is in SHADER_READ_ONLY_OPTIMAL — encode it.
    let packets = encoder.encode_image(bgra_view, timestamp_ns)?;
    Ok(packets)
}

// ---------------------------------------------------------------------------
// Timestamp helper (no chrono dependency)
// ---------------------------------------------------------------------------

fn chrono_timestamp() -> String {
    let output = Command::new("date")
        .arg("+%Y%m%d_%H%M%S")
        .output()
        .expect("failed to run date");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
