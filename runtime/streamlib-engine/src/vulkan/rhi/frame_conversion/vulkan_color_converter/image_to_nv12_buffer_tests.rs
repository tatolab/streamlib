// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::color::{MatrixId, PrimariesId, RangeId, rgb_to_yuv_matrix};
use crate::core::context::GpuContext;
use crate::core::rhi::VulkanLayout;
use crate::vulkan::rhi::{VulkanAccess, VulkanStage};

/// Where the NV12 destination's planes sit: past a header, at strides
/// padded beyond the width the way an IOSurface's are.
struct Nv12DestinationShape {
    layout: SourceLayoutInfo,
    byte_len: usize,
}

fn nv12_destination_shape(width: u32, height: u32) -> Nv12DestinationShape {
    let stride = width.next_multiple_of(64);
    let luma_plane_bytes = stride * height;
    let layout = SourceLayoutInfo::nv12_starting_at(0, stride, stride, luma_plane_bytes);
    Nv12DestinationShape {
        layout,
        byte_len: (luma_plane_bytes + stride * height.div_ceil(2)) as usize,
    }
}

/// The CPU reference: the matrix table the kernel is pushed, luma per
/// pixel, chroma the box average of each 2×2 block with edge pixels
/// repeated, laid out as `shape` says.
fn nv12_reference(
    rgba: &[u8],
    width: u32,
    height: u32,
    shape: &Nv12DestinationShape,
    info: &ResolvedColorInfo,
) -> (Vec<Option<u8>>, Vec<Option<u8>>) {
    let d = rgb_to_yuv_matrix(info.matrix, info.range);
    let m = d.matrix_row_major;
    let ycbcr = |x: u32, y: u32| -> [f32; 3] {
        let (x, y) = (x.min(width - 1), y.min(height - 1));
        let i = ((y * width + x) * 4) as usize;
        let (r, g, b) = (rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32);
        [
            (m[0] * r + m[1] * g + m[2] * b + d.offset[0]).clamp(0.0, 255.0),
            (m[3] * r + m[4] * g + m[5] * b + d.offset[1]).clamp(0.0, 255.0),
            (m[6] * r + m[7] * g + m[8] * b + d.offset[2]).clamp(0.0, 255.0),
        ]
    };
    let SourceLayoutInfo {
        plane0_stride_bytes,
        plane1_stride_bytes,
        ..
    } = shape.layout;
    let mut luma = vec![None; (plane0_stride_bytes * height) as usize];
    for y in 0..height {
        for x in 0..width {
            luma[(y * plane0_stride_bytes + x) as usize] = Some(ycbcr(x, y)[0].round() as u8);
        }
    }
    let mut chroma = vec![None; (plane1_stride_bytes * height.div_ceil(2)) as usize];
    for chroma_y in 0..height.div_ceil(2) {
        for chroma_x in 0..width.div_ceil(2) {
            let (x, y) = (chroma_x * 2, chroma_y * 2);
            let block = [
                ycbcr(x, y),
                ycbcr(x + 1, y),
                ycbcr(x, y + 1),
                ycbcr(x + 1, y + 1),
            ];
            let average = |axis: usize| {
                (block.iter().map(|sample| sample[axis]).sum::<f32>() / 4.0).round() as u8
            };
            let o = (chroma_y * plane1_stride_bytes + chroma_x * 2) as usize;
            chroma[o] = Some(average(1));
            chroma[o + 1] = Some(average(2));
        }
    }
    (luma, chroma)
}

/// Synthetic RGBA against the CPU conversion, at an extent that is odd on
/// both axes and a multiple of neither the block nor the workgroup: every
/// luma and chroma sample of the frame lands within one step of rounding.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a Vulkan device"
)]
#[test]
fn the_nv12_pass_writes_every_sample_of_both_planes() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(gpu) => gpu,
        Err(e) => {
            tracing::warn!("skipping — no GPU device: {e}");
            return;
        }
    };
    let (width, height) = (37u32, 9u32);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (i as u32) % width;
        let y = (i as u32) / width;
        px[0] = (x * 7 + y * 3) as u8;
        px[1] = (255 - x * 5) as u8;
        px[2] = (y * 29 + 11) as u8;
        px[3] = 255;
    }
    let (published, pixel_buffer) = gpu
        .acquire_pixel_buffer(width, height, PixelFormat::Rgba32)
        .expect("pixel buffer");
    pixel_buffer
        .write_this_plane_from(0, &rgba)
        .expect("the pattern staged");
    let surface_id = published.to_string();
    gpu.upload_pixel_buffer_as_texture(&surface_id, &pixel_buffer, width, height)
        .expect("upload");
    let registration = gpu
        .resolve_texture_registration_by_surface_id(&surface_id, None, width, height)
        .expect("registration");

    for (range, destination_format) in [
        (RangeId::Limited, PixelFormat::Nv12VideoRange),
        (RangeId::Full, PixelFormat::Nv12FullRange),
    ] {
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Srgb,
            matrix: MatrixId::Bt709,
            range,
        };
        let shape = nv12_destination_shape(width, height);
        let destination = gpu
            .acquire_storage_buffer(
                &crate::core::rhi::TensorStorageBufferLayout::of_bytes(shape.byte_len as u64)
                    .expect("a non-zero destination"),
            )
            .expect("a host-visible destination");
        // SAFETY: the buffer is host-mapped and `byte_len` long.
        unsafe { std::ptr::write_bytes(destination.mapped_ptr(), 0, shape.byte_len) };
        let converter = gpu
            .color_converter(PixelFormat::Rgba32, destination_format)
            .expect("RGBA→NV12 converter");
        let kernel = converter
            .prepare_image_to_nv12_buffer(registration.texture(), &destination, shape.layout, &info)
            .expect("prepare");
        let mut recorder = gpu
            .create_command_recorder("nv12_pass_test")
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
                image_to_nv12_buffer_dispatch_group_counts(width, height).0,
                image_to_nv12_buffer_dispatch_group_counts(width, height).1,
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
        registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);

        // SAFETY: the submission was waited for; the mapping is
        // `byte_len` long.
        let written =
            unsafe { std::slice::from_raw_parts(destination.mapped_ptr(), shape.byte_len) };
        let (luma_plane, chroma_plane) =
            written.split_at(shape.layout.plane1_offset_bytes as usize);
        let (expected_luma, expected_chroma) = nv12_reference(&rgba, width, height, &shape, &info);
        for (plane_name, got_plane, expected_plane) in [
            ("luma", luma_plane, &expected_luma),
            ("chroma", chroma_plane, &expected_chroma),
        ] {
            for (i, (&got, want)) in got_plane.iter().zip(expected_plane.iter()).enumerate() {
                if let Some(want) = want {
                    assert!(
                        got.abs_diff(*want) <= 1,
                        "{range:?} {plane_name} byte {i}: GPU {got} vs CPU {want}"
                    );
                }
            }
        }
    }
}
