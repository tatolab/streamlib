// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::color::{MatrixId, PrimariesId, RangeId, rgb_to_yuv_matrix};
use crate::core::context::GpuContext;
use crate::core::rhi::VulkanLayout;
use crate::vulkan::rhi::{VulkanAccess, VulkanStage};

/// The CPU reference: the same matrix table the kernel is pushed, one
/// macropixel at a time, chroma averaged across the pair, rows laid out
/// at `stride_bytes` with the pad left zero.
fn yuyv_reference(
    rgba: &[u8],
    width: u32,
    height: u32,
    stride_bytes: u32,
    info: &ResolvedColorInfo,
) -> Vec<u8> {
    let d = rgb_to_yuv_matrix(info.matrix, info.range);
    let m = d.matrix_row_major;
    let ycbcr = |x: u32, y: u32| -> [f32; 3] {
        let i = ((y * width + x) * 4) as usize;
        let (r, g, b) = (rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32);
        [
            (m[0] * r + m[1] * g + m[2] * b + d.offset[0]).clamp(0.0, 255.0),
            (m[3] * r + m[4] * g + m[5] * b + d.offset[1]).clamp(0.0, 255.0),
            (m[6] * r + m[7] * g + m[8] * b + d.offset[2]).clamp(0.0, 255.0),
        ]
    };
    let mut out = vec![0u8; (stride_bytes * height) as usize];
    for y in 0..height {
        for pair in 0..width.div_ceil(2) {
            let x0 = pair * 2;
            let x1 = (x0 + 1).min(width - 1);
            let (l, r) = (ycbcr(x0, y), ycbcr(x1, y));
            let o = (y * stride_bytes + pair * 4) as usize;
            out[o] = l[0].round() as u8;
            out[o + 1] = (0.5 * (l[1] + r[1])).round() as u8;
            out[o + 2] = r[0].round() as u8;
            out[o + 3] = (0.5 * (l[2] + r[2])).round() as u8;
        }
    }
    out
}

/// Synthetic RGBA against the CPU conversion: every macropixel of the
/// target range is written, to within one step of rounding, and the row
/// pad and the guard band past the frame stay untouched.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a Vulkan device"
)]
#[test]
fn the_yuyv_pass_writes_every_pixel_of_the_target_range() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(gpu) => gpu,
        Err(e) => {
            tracing::warn!("skipping — no GPU device: {e}");
            return;
        }
    };
    // A width that is not a workgroup multiple, and a destination stride
    // padded past 2·width the way a driver's `bytesperline` may be: the
    // pad inside every row and the guard band past the frame stay zero.
    let (width, height) = (34u32, 7u32);
    let stride_bytes = (width * 2).next_multiple_of(64);
    assert!(stride_bytes > width * 2);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (i as u32) % width;
        let y = (i as u32) / width;
        px[0] = (x * 7 + y * 3) as u8;
        px[1] = (255 - x * 5) as u8;
        px[2] = (y * 37 + 11) as u8;
        px[3] = 255;
    }
    let (published, pixel_buffer) = gpu
        .acquire_pixel_buffer(width, height, PixelFormat::Rgba32)
        .expect("pixel buffer");
    unsafe {
        std::ptr::copy_nonoverlapping(
            rgba.as_ptr(),
            pixel_buffer.plane_base_address(0).cast::<u8>(),
            rgba.len(),
        );
    }
    let surface_id = published.to_string();
    gpu.upload_pixel_buffer_as_texture(&surface_id, &pixel_buffer, width, height)
        .expect("upload");
    let registration = gpu
        .resolve_texture_registration_by_surface_id(&surface_id, None, width, height)
        .expect("registration");

    let info = ResolvedColorInfo {
        primaries: PrimariesId::Bt709,
        transfer: TransferId::Srgb,
        matrix: MatrixId::Smpte170m,
        range: RangeId::Limited,
    };
    let converter = gpu
        .color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
        .expect("RGBA→YUYV converter");

    // A guard band past the frame: the pass writes only the frame.
    let destination_byte_len = ((stride_bytes * height) as usize).next_multiple_of(4096);
    let destination = gpu
        .acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(destination_byte_len as u64)
                .expect("a non-zero destination"),
        )
        .expect("a host-visible destination");
    // SAFETY: the buffer is host-mapped and `destination_byte_len` long.
    unsafe { std::ptr::write_bytes(destination.mapped_ptr(), 0, destination_byte_len) };

    let kernel = converter
        .prepare_image_to_yuyv_buffer(registration.texture(), &destination, stride_bytes, &info)
        .expect("prepare");
    let mut recorder = gpu
        .create_command_recorder("yuyv_pass_test")
        .expect("recorder");
    recorder.begin().expect("begin");
    recorder
        .record_image_barrier(
            registration.texture(),
            registration.current_layout(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::COMPUTE_SHADER,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::SHADER_SAMPLED_READ,
        )
        .expect("image barrier");
    recorder
        .record_dispatch(
            &kernel,
            width.div_ceil(2).div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
            height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
            1,
        )
        .expect("dispatch");
    recorder
        .record_buffer_barrier(
            &destination,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::HOST,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::HOST_READ,
        )
        .expect("release to host");
    recorder.submit_and_wait().expect("submit");

    // SAFETY: the submission was waited for; the mapping is
    // `destination_byte_len` long.
    let whole_destination =
        unsafe { std::slice::from_raw_parts(destination.mapped_ptr(), destination_byte_len) };
    let expected = yuyv_reference(&rgba, width, height, stride_bytes, &info);
    let written = &whole_destination[..expected.len()];
    let mut worst = 0u8;
    for (i, (&got, &want)) in written.iter().zip(&expected).enumerate() {
        let in_row_pad = (i as u32 % stride_bytes) >= width * 2;
        if in_row_pad {
            assert_eq!(got, 0, "byte {i} is row padding and must stay untouched");
            continue;
        }
        let diff = got.abs_diff(want);
        assert!(diff <= 1, "byte {i}: GPU {got} vs CPU {want}");
        worst = worst.max(diff);
    }
    assert!(
        whole_destination[expected.len()..].iter().all(|&b| b == 0),
        "nothing past the frame is written"
    );
    tracing::info!(
        worst_byte_difference = worst,
        "YUYV pass matches the CPU reference"
    );
}
