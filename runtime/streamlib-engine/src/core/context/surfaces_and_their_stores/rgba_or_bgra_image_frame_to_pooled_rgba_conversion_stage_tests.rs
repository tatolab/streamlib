// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_ipc_types::{ImageFrameDescriptorBody, ImageFrameH273Colour};

use super::rgba_or_bgra_image_frame_to_pooled_rgba_conversion_stage::{
    RgbaOrBgraImageFrameToPooledRgbaConversionStage, texture_format_the_kernel_samples,
};
use crate::core::context::GpuContext;
use crate::core::rhi::{PixelFormat, TextureFormat};
use crate::core::test_support::LiveSurfaceShareServiceForATest;

/// A frame of a format the kernel would not sample as it is stored, or no format at all, is
/// refused naming it and the two it takes.
#[test]
fn a_frame_of_a_format_the_kernel_does_not_sample_as_stored_is_refused_naming_it() {
    for (format_wire_value, named) in [
        (TextureFormat::Rgba16Float as u32, "rgba16_float"),
        (TextureFormat::Bgra8UnormSrgb as u32, "bgra8_unorm_srgb"),
        (TextureFormat::Nv12 as u32, "nv12"),
        (9_999, "an unknown texture format (9999)"),
    ] {
        let refusal = texture_format_the_kernel_samples(format_wire_value)
            .expect_err("the kernel does not sample it as stored")
            .to_string();
        assert!(refusal.contains(named), "{refusal}");
        assert!(refusal.contains("rgba8_unorm and bgra8_unorm"), "{refusal}");
    }
    for format in [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm] {
        assert_eq!(
            texture_format_the_kernel_samples(format as u32).expect("the kernel samples it"),
            format
        );
    }
}

/// A BGRA frame from a native node's output pool converts into an `rgba8_unorm` frame from the
/// converting node's own pool, each pixel's channels in RGBA order, at an extent no workgroup
/// divides; the frame keeps its colour.
///
/// GPU-gated: skips when no device is present.
#[test]
fn a_bgra_frame_converts_into_an_rgba8_unorm_frame_in_rgba_order() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the BGRA to RGBA frame conversion test skips");
        return;
    };
    let _live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let (width, height) = (34u32, 7u32);
    let bgra: Vec<u8> = (0..width * height)
        .flat_map(|pixel| {
            let (x, y) = (pixel % width, pixel / width);
            [
                (x * 7 + y * 3) as u8,
                (255 - x * 5) as u8,
                (y * 37 + 11) as u8,
                (128 + x) as u8,
            ]
        })
        .collect();
    let mut producer_pool =
        gpu.create_node_output_pixel_buffer_pool_for_a_native_node("a-bgra-producer");
    let produced = producer_pool
        .acquire_the_next_output_pixel_buffer(width, height, PixelFormat::Bgra32)
        .expect("the producer's frame");
    produced
        .pixel_buffer
        .write_this_plane_from(0, &bgra)
        .expect("the producer writes its pixels");
    let colour = ImageFrameH273Colour {
        primaries: 1,
        transfer: 13,
        matrix: 0,
        range: 1,
    };
    let mut stage = RgbaOrBgraImageFrameToPooledRgbaConversionStage::create_on(
        &gpu,
        "a-texture-format-convert",
    )
    .expect("the stage");

    let converted = stage
        .convert_on(
            &gpu,
            &ImageFrameDescriptorBody {
                slot: produced
                    .gpu_pool_slot_frame
                    .expect("the producer's frame leases a claim-table row"),
                width,
                height,
                format: TextureFormat::Bgra8Unorm as u32,
                colour,
            },
        )
        .expect("a BGRA frame converts");

    assert_eq!(
        (converted.width, converted.height, converted.format),
        (width, height, TextureFormat::Rgba8Unorm as u32)
    );
    assert_eq!(converted.colour, colour);
    let converted_surface_id = gpu
        .published_frame_id_of_the_gpu_pool_slot_frame(&converted.slot)
        .expect("the converted frame names a slot of this runtime");
    let converted_pixel_buffer = gpu
        .get_pixel_buffer(&converted_surface_id)
        .expect("the converted frame's pixel buffer");
    let rgba = unsafe {
        std::slice::from_raw_parts(converted_pixel_buffer.plane_base_address(0), bgra.len())
    };
    let expected: Vec<u8> = bgra
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[blue, green, red, alpha]| [red, green, blue, alpha])
        .collect();
    assert_eq!(rgba, expected.as_slice());
}
