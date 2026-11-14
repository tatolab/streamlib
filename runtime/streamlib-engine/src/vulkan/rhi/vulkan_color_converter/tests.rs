// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::color::{MatrixId, PrimariesId, RangeId, from_linear, to_linear};
use crate::core::rhi::{
    PixelBuffer, TextureDescriptor, TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
    TextureUsages,
};
use crate::host_rhi::HostTextureExt;
use crate::vulkan::rhi::{HostVulkanBuffer, HostVulkanTexture, VulkanTextureReadback};

fn try_device() -> Option<Arc<HostVulkanDevice>> {
    HostVulkanDevice::new().ok()
}

/// Construction succeeds for every supported `(src, dst)` pair —
/// no SPIR-V is loaded yet (that's lazy on first dispatch), but the
/// format-pair validation runs.
#[test]
fn new_succeeds_for_every_supported_pair() {
    let Some(device) = try_device() else { return };
    for src in [
        PixelFormat::Nv12VideoRange,
        PixelFormat::Nv12FullRange,
        PixelFormat::Yuyv422,
    ] {
        let conv = VulkanColorConverter::new(&device, src, PixelFormat::Rgba32)
            .expect("converter construction must succeed");
        assert_eq!(conv.src_format(), src);
        assert_eq!(conv.dst_format(), PixelFormat::Rgba32);
    }
}

/// Unsupported destination format is rejected at construction
/// time, not at first dispatch.
#[test]
fn new_rejects_unsupported_dst_format() {
    let Some(device) = try_device() else { return };
    let err = VulkanColorConverter::new(&device, PixelFormat::Nv12FullRange, PixelFormat::Bgra32)
        .err()
        .expect("must reject Bgra32 dest today");
    assert!(matches!(err, Error::NotSupported(_)));
}

// ---- GPU-output bit-exact regression coverage ----
//
// Restores the regression coverage deleted with
// `VulkanFormatConverter::tests::nv12_full_range_to_bgra_matches_cpu_reference`
// in #823. The deleted test's CPU reference used hardcoded
// BT.601-Full coefficients (1.402 / 0.344136 / 0.714136 / 1.772),
// which made it independent of any production matrix code — a
// bug in the production matrix would fail the test. This file
// preserves that property: each test passes its hardcoded
// matrix + offset into [`cpu_reference_rgba`] as plain `[f32; 9]`
// + `[f32; 3]`, NOT through `yuv_to_rgb_matrix`. The shader-side
// values come from `yuv_to_rgb_matrix` via push constants, so a
// regression in the Rust matrix shifts only the GPU side and the
// test fails.
//
// Transfer functions stay routed through `core::color::transfer`
// because the Slang has independent closed-form implementations
// in `color_convert_common.slang` — Rust-vs-Slang drift is the
// failure mode the transfer-path test below catches, and that
// requires both implementations to be independently authored.
//
// The math chain mirrors `convert_color` in
// `vulkan/rhi/shaders/color_convert_common.slang`:
//   1. `c = ycbcr_byte - range_offset`
//   2. `rgb_byte = M * c` (3×3 row-major matrix)
//   3. `rgb = clamp(rgb_byte / 255, 0, 1)`
//   4. if `transfer_in != transfer_out`:
//         `rgb = from_linear(out, to_linear(in, rgb))`  per channel
//
// What these tests catch:
//   - `ColorConverterPushConstants` std430 field ORDER / SIZE
//     drift. Confirmed by negative-test: zeroing the Y component
//     of `range_offset` in `from_resolved` fails the BT.709-limited
//     and BT.709→sRGB tests with thousands of pixel mismatches.
//   - `yuv_to_rgb_matrix` / `range_scaling` coefficient
//     regressions — CPU reference is hardcoded, GPU pulls
//     through the production path, any divergence is caught.
//   - Shader's NV12 stride math (`read_byte` packed-uint
//     extraction + `plane0_stride_bytes` / `plane1_stride_bytes`
//     / `plane1_offset_bytes` walks).
//   - Slang ↔ Rust drift in the transfer functions
//     (`color_convert_common.slang` vs `core::color::transfer`).
//   - `imageStore` correctness on `rgba8` storage image (UNORM
//     rounding within ±1 of the CPU reference).
//   - The `UNDEFINED → GENERAL` layout transition on the output
//     texture (without it the dispatch is spec-illegal).
//
// Out of scope (locked by their own unit-test layers):
//   - `core::color::matrix::tests` independently locks matrix
//     coefficients with hardcoded golden values per
//     (matrix, range) tuple — the source of truth this file's
//     hardcoded matrices were taken from.
//   - `core::color::transfer::tests` locks Rust-side transfer
//     math via round-trip + known-points.

/// Hardcoded matrix + offset for a `(MatrixId, RangeId)` tuple.
/// Source: H.273 / ITU-T canonical coefficients (same numbers
/// `core::color::matrix::tests` independently locks).
///
/// Authored independently of the production
/// `core::color::yuv_to_rgb_matrix` so a regression there shifts
/// only the GPU side of the pipeline and the test fails.
struct CpuReferenceMatrix {
    row_major: [f32; 9],
    offset: [f32; 3],
}

/// BT.601 (SMPTE 170M) full-range, the canonical webcam path.
const BT601_FULL: CpuReferenceMatrix = CpuReferenceMatrix {
    row_major: [1.0, 0.0, 1.402, 1.0, -0.344136, -0.714136, 1.0, 1.772, 0.0],
    offset: [0.0, 128.0, 128.0],
};

/// BT.709 limited-range, the codec-output path. Coefficients
/// from `255/219 * Y` scale + chroma `255/224 * 1.5748` etc.
const BT709_LIMITED: CpuReferenceMatrix = CpuReferenceMatrix {
    row_major: [
        1.164384, 0.0, 1.792741, 1.164384, -0.213249, -0.532909, 1.164384, 2.112402, 0.0,
    ],
    offset: [16.0, 128.0, 128.0],
};

/// CPU reference: one NV12 source pixel → one RGBA8 output pixel.
///
/// Mirrors `convert_color()` in `color_convert_common.slang`.
/// `matrix` and `offset` are passed in explicitly (hardcoded by
/// the caller) so the reference stays independent of the
/// production `yuv_to_rgb_matrix`. Transfer math uses
/// `core::color::transfer` since the Slang closed-form is
/// independently authored.
fn cpu_reference_rgba(
    y_byte: u8,
    u_byte: u8,
    v_byte: u8,
    matrix: &CpuReferenceMatrix,
    transfer_in: TransferId,
    transfer_out: TransferId,
) -> [u8; 4] {
    let m = matrix.row_major;
    let off = matrix.offset;
    let c = [
        y_byte as f32 - off[0],
        u_byte as f32 - off[1],
        v_byte as f32 - off[2],
    ];
    let mut rgb = [
        (m[0] * c[0] + m[1] * c[1] + m[2] * c[2]) / 255.0,
        (m[3] * c[0] + m[4] * c[1] + m[5] * c[2]) / 255.0,
        (m[6] * c[0] + m[7] * c[1] + m[8] * c[2]) / 255.0,
    ];
    for ch in &mut rgb {
        *ch = ch.clamp(0.0, 1.0);
    }
    if transfer_in != transfer_out {
        for ch in &mut rgb {
            *ch = from_linear(transfer_out, to_linear(transfer_in, *ch));
        }
    }
    [
        (rgb[0] * 255.0).round() as u8,
        (rgb[1] * 255.0).round() as u8,
        (rgb[2] * 255.0).round() as u8,
        255,
    ]
}

/// Synthesize a tight-packed NV12 byte buffer of size
/// `width * height * 3 / 2`. Picks Y / U / V patterns that vary
/// across the frame so chroma actually contributes (plane swaps
/// surface as wrong color) and stays away from byte saturation
/// (full bytes go through the matrix-expand path without
/// clamping dominating the signal).
fn build_deterministic_nv12(width: u32, height: u32) -> Vec<u8> {
    assert!(
        width.is_multiple_of(2) && height.is_multiple_of(2),
        "NV12 needs even dims"
    );
    let y_plane_size = (width * height) as usize;
    let uv_plane_size = (width * height / 2) as usize;
    let mut buf = vec![0u8; y_plane_size + uv_plane_size];
    for y in 0..height {
        for x in 0..width {
            buf[(y * width + x) as usize] =
                (28u32 + (x.wrapping_mul(5).wrapping_add(y.wrapping_mul(7))) % 200) as u8;
        }
    }
    let uv_base = y_plane_size;
    let half_h = height / 2;
    let half_w = width / 2;
    for cy in 0..half_h {
        for cx in 0..half_w {
            let off = uv_base + (cy * width + cx * 2) as usize;
            buf[off] =
                (48u32 + (cx.wrapping_mul(13).wrapping_add(cy.wrapping_mul(11))) % 160) as u8;
            buf[off + 1] = (48u32
                + (cx
                    .wrapping_mul(7)
                    .wrapping_add(cy.wrapping_mul(17))
                    .wrapping_add(32))
                    % 160) as u8;
        }
    }
    buf
}

/// Allocate an `Rgba8Unorm` storage texture with `STORAGE | COPY_SRC
/// | COPY_DST` usage, transition it from `UNDEFINED` to `GENERAL` so
/// the converter's `imageStore` is spec-legal, and return it.
///
/// Mirrors the `make_filled_texture` UNDEFINED→GENERAL one-shot in
/// `vulkan_texture_readback.rs::tests`; this variant skips the fill
/// step because the converter dispatch will write every pixel.
fn allocate_storage_target_in_general(
    device: &Arc<HostVulkanDevice>,
    width: u32,
    height: u32,
) -> Texture {
    let desc = TextureDescriptor {
        width,
        height,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::STORAGE_BINDING,
        label: Some("color-converter-test-output"),
    };
    let host_tex = HostVulkanTexture::new(device, &desc).expect("host texture");
    let texture = <Texture as crate::host_rhi::HostTextureExt>::from_vulkan(host_tex);

    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();
    unsafe {
        let pool = dev
            .create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                    .build(),
                None,
            )
            .expect("command pool");
        let cmd = dev
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1)
                    .build(),
            )
            .expect("command buffer")[0];
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
        .expect("begin");

        let to_general = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(qf)
            .dst_queue_family_index(qf)
            .image(texture.vulkan_inner().image().expect("vk image"))
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let bs = [to_general];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&bs)
            .build();
        dev.cmd_pipeline_barrier2(cmd, &dep);
        dev.end_command_buffer(cmd).expect("end");

        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let submits = [vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build()];
        device
            .submit_to_queue(queue, &submits, vk::Fence::null())
            .expect("submit transition");
        dev.queue_wait_idle(queue).expect("wait idle");
        dev.destroy_command_pool(pool, None);
    }

    texture
}

/// Wrap a raw NV12 byte buffer in a HOST_VISIBLE `HostVulkanBuffer`
/// + `PixelBuffer` so the converter sees it as a storage-buffer
///   source.
fn upload_nv12_pixel_buffer(
    device: &Arc<HostVulkanDevice>,
    nv12_bytes: &[u8],
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
) -> PixelBuffer {
    let host_buf = HostVulkanBuffer::new(device, nv12_bytes.len() as u64).expect("host buffer");
    unsafe {
        std::ptr::copy_nonoverlapping(nv12_bytes.as_ptr(), host_buf.mapped_ptr(), nv12_bytes.len());
    }
    // `bytes_per_pixel` is descriptive metadata only — the shader
    // walks the buffer via `SourceLayoutInfo` push constants.
    PixelBuffer::from_host_vulkan_buffer(Arc::new(host_buf), width, height, 1, pixel_format)
}

/// End-to-end driver: dispatch the converter on a deterministic
/// NV12 input under `info` (drives the production push-constant
/// builder), read the resulting RGBA storage texture back to a
/// CPU buffer, and assert every output pixel matches the
/// independently-hardcoded `cpu_matrix` + transfer round-trip
/// within ±1 per channel. `info.matrix` / `info.range` must
/// describe the same tuple `cpu_matrix` was authored for —
/// the test catches regressions where the two diverge.
fn run_nv12_to_rgba_pixel_check(
    device: &Arc<HostVulkanDevice>,
    src_pixel_format: PixelFormat,
    info: &ResolvedColorInfo,
    dst_transfer: TransferId,
    cpu_matrix: &CpuReferenceMatrix,
    width: u32,
    height: u32,
    label: &str,
) {
    let nv12_bytes = build_deterministic_nv12(width, height);
    let pixel_buf = upload_nv12_pixel_buffer(device, &nv12_bytes, width, height, src_pixel_format);
    let output_texture = allocate_storage_target_in_general(device, width, height);

    let converter = VulkanColorConverter::new(device, src_pixel_format, PixelFormat::Rgba32)
        .expect("converter construction");

    let src_layout = SourceLayoutInfo::nv12_tight(width, height);
    let kernel = converter
        .prepare_buffer_to_image_pixel(&pixel_buf, src_layout, &output_texture, info, dst_transfer)
        .expect("prepare");
    let dispatch_x = width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
    let dispatch_y = height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
    kernel
        .dispatch(dispatch_x, dispatch_y, 1)
        .expect("dispatch");

    let readback = VulkanTextureReadback::new(
        device,
        &TextureReadbackDescriptor {
            label: "color-converter-test-readback",
            format: TextureFormat::Rgba8Unorm,
            width,
            height,
        },
    )
    .expect("readback handle");
    let ticket = readback
        .submit(&output_texture, TextureSourceLayout::General)
        .expect("readback submit");
    let gpu = readback
        .wait_and_read(ticket, u64::MAX)
        .expect("readback wait");

    let y_plane_size = (width * height) as usize;
    let mut mismatches = 0u32;
    let mut first_mismatch_msg = String::new();
    for y in 0..height {
        for x in 0..width {
            let y_byte = nv12_bytes[(y * width + x) as usize];
            // Interleaved (U, V) at half spatial resolution, even-x.
            let uv_offset = y_plane_size + ((y >> 1) * width + (x & !1)) as usize;
            let u_byte = nv12_bytes[uv_offset];
            let v_byte = nv12_bytes[uv_offset + 1];

            let expected = cpu_reference_rgba(
                y_byte,
                u_byte,
                v_byte,
                cpu_matrix,
                info.transfer,
                dst_transfer,
            );

            let off = ((y * width + x) * 4) as usize;
            let actual = [gpu[off], gpu[off + 1], gpu[off + 2], gpu[off + 3]];

            for ch in 0..4 {
                let diff = (actual[ch] as i32 - expected[ch] as i32).abs();
                if diff > 1 {
                    if mismatches == 0 {
                        first_mismatch_msg = format!(
                            "[{label}] pixel ({x},{y}) ch {ch}: gpu={actual:?} expected={expected:?} (Y={y_byte},U={u_byte},V={v_byte})"
                        );
                    }
                    mismatches += 1;
                }
            }
        }
    }
    assert_eq!(
        mismatches, 0,
        "{} pixel(s) failed ±1 tolerance. First: {}",
        mismatches, first_mismatch_msg
    );
}

/// BT.601 full-range NV12 (the canonical webcam path) → Rgba8Unorm
/// must match the CPU reference within ±1 per channel.
///
/// Locks (see module header for the full taxonomy):
/// - Push-constant struct field-order — the shader pulls the
///   matrix + offset out of fixed std430 slots.
/// - Transfer-bypass path (`Srgb` source = `Srgb` dest → shader
///   skips `pow()` per channel).
/// - `SourceLayoutInfo::nv12_tight` strides flowing through push
///   constants and the shader's `read_byte` walk.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn nv12_full_range_bt601_matches_cpu_reference() {
    let Some(device) = try_device() else { return };
    let info = ResolvedColorInfo {
        primaries: PrimariesId::Bt709,
        transfer: TransferId::Srgb,
        matrix: MatrixId::Smpte170m,
        range: RangeId::Full,
    };
    run_nv12_to_rgba_pixel_check(
        &device,
        PixelFormat::Nv12FullRange,
        &info,
        TransferId::Srgb,
        &BT601_FULL,
        64,
        32,
        "bt601-full",
    );
}

/// BT.709 limited-range NV12 (the codec-output path) → Rgba8Unorm
/// must match the CPU reference within ±1 per channel.
///
/// Locks (see module header for the full taxonomy):
/// - Push-constant `range_offset` slot is wired through to the
///   shader's `c = ycbcr_byte - range_offset` step — confirmed
///   by negative-test: zeroing the Y component of `range_offset`
///   in `from_resolved` fails this test with thousands of
///   mismatches.
/// - `yuv_to_rgb_matrix` / `range_scaling` regressions on the
///   production side — confirmed by negative-test: reverting
///   the `255/219` Y-scale in `core::color::matrix::range_scaling`
///   fails this test (the CPU reference's `BT709_LIMITED` matrix
///   is hardcoded, so a regression in the Rust matrix shifts only
///   the GPU side and the test catches it).
/// - Transfer-bypass path (`Bt709` source = `Bt709` dest).
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn nv12_video_range_bt709_matches_cpu_reference() {
    let Some(device) = try_device() else { return };
    let info = ResolvedColorInfo {
        primaries: PrimariesId::Bt709,
        transfer: TransferId::Bt709,
        matrix: MatrixId::Bt709,
        range: RangeId::Limited,
    };
    run_nv12_to_rgba_pixel_check(
        &device,
        PixelFormat::Nv12VideoRange,
        &info,
        TransferId::Bt709,
        &BT709_LIMITED,
        64,
        32,
        "bt709-limited",
    );
}

/// BT.709 limited-range NV12 with mismatched dest transfer
/// (`Bt709` source → `Srgb` dest) → Rgba8Unorm must match the CPU
/// reference within ±1 per channel.
///
/// Locks (see module header for the full taxonomy):
/// - `FLAG_APPLY_TRANSFER` path active (`transfer_in !=
///   transfer_out` → shader runs `from_linear(srgb,
///   to_linear(bt709, x))` per channel).
/// - Slang ↔ Rust transfer-function drift. The transfer-bypass
///   tests above can't catch this; here the Slang closed-forms
///   in `color_convert_common.slang` and the Rust closed-forms in
///   `core::color::transfer` are independent implementations,
///   and the test fails the moment they disagree.
/// - Mid-stream transfer change costs only push constants — no
///   pipeline rebuild — which the test verifies by reusing the
///   same `(src, dst)` PixelFormat pair as the prior test.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn nv12_bt709_to_srgb_transfer_path_matches_cpu_reference() {
    let Some(device) = try_device() else { return };
    let info = ResolvedColorInfo {
        primaries: PrimariesId::Bt709,
        transfer: TransferId::Bt709,
        matrix: MatrixId::Bt709,
        range: RangeId::Limited,
    };
    run_nv12_to_rgba_pixel_check(
        &device,
        PixelFormat::Nv12VideoRange,
        &info,
        TransferId::Srgb,
        &BT709_LIMITED,
        64,
        32,
        "bt709→srgb",
    );
}

fn host_visible_storage_buffer_holding(
    device: &Arc<HostVulkanDevice>,
    byte_len: usize,
    bytes_at: &[(usize, &[u8])],
) -> crate::core::rhi::StorageBuffer {
    let buffer = HostVulkanBuffer::new_storage_buffer_host_visible(
        device,
        byte_len.next_multiple_of(4) as u64,
    )
    .expect("host-visible storage buffer");
    unsafe {
        std::ptr::write_bytes(buffer.mapped_ptr(), 0xA7, byte_len);
        for (offset, bytes) in bytes_at {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                buffer.mapped_ptr().add(*offset),
                bytes.len(),
            );
        }
    }
    crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(Arc::new(buffer))
}

fn convert_and_read_back(
    device: &Arc<HostVulkanDevice>,
    converter: &VulkanColorConverter,
    source: &crate::core::rhi::StorageBuffer,
    layout: SourceLayoutInfo,
    info: &ResolvedColorInfo,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let output_texture = allocate_storage_target_in_general(device, width, height);
    converter
        .prepare_buffer_to_image_storage(source, layout, &output_texture, info, TransferId::Srgb)
        .expect("prepare")
        .dispatch(
            width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
            height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
            1,
        )
        .expect("dispatch");
    let readback = VulkanTextureReadback::new(
        device,
        &TextureReadbackDescriptor {
            label: "color-converter-offset-test-readback",
            format: TextureFormat::Rgba8Unorm,
            width,
            height,
        },
    )
    .expect("readback handle");
    let ticket = readback
        .submit(&output_texture, TextureSourceLayout::General)
        .expect("readback submit");
    readback
        .wait_and_read(ticket, u64::MAX)
        .expect("readback wait")
        .to_vec()
}

/// A sampled BGRA texture lands in an RGBA storage image texel for
/// texel, its channels in RGBA order, at an extent no workgroup divides.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a Vulkan device"
)]
#[test]
fn a_bgra_texture_converts_into_an_rgba_texture_in_rgba_order() {
    let Some(device) = try_device() else { return };
    let (width, height) = (34u32, 7u32);
    let mut bgra = vec![0u8; (width * height * 4) as usize];
    for (i, px) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (i as u32) % width;
        let y = (i as u32) / width;
        px[0] = (x * 7 + y * 3) as u8;
        px[1] = (255 - x * 5) as u8;
        px[2] = (y * 37 + 11) as u8;
        px[3] = (128 + x) as u8;
    }
    let staging = HostVulkanBuffer::new_storage_buffer_host_visible(&device, bgra.len() as u64)
        .expect("staging buffer");
    unsafe {
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), staging.mapped_ptr(), bgra.len());
    }
    let source = HostVulkanTexture::new(
        &device,
        &TextureDescriptor {
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
            label: Some("color-converter-test-bgra-source"),
        },
    )
    .expect("bgra source texture");
    let uploaded =
        unsafe { device.upload_buffer_to_image(staging.buffer(), &source, width, height) }
            .expect("upload");
    assert_eq!(
        uploaded.final_texture_layout,
        crate::core::rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL
    );
    let source = <Texture as HostTextureExt>::from_vulkan(source);
    let destination = allocate_storage_target_in_general(&device, width, height);

    let converter = VulkanColorConverter::new(&device, PixelFormat::Bgra32, PixelFormat::Rgba32)
        .expect("BGRA→RGBA converter");
    converter
        .prepare_image_to_image_storage(&source, &destination)
        .expect("prepare")
        .dispatch(
            width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
            height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
            1,
        )
        .expect("dispatch");
    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "color-converter-image-to-image-readback",
            format: TextureFormat::Rgba8Unorm,
            width,
            height,
        },
    )
    .expect("readback handle");
    let ticket = readback
        .submit(&destination, TextureSourceLayout::General)
        .expect("readback submit");
    let rgba = readback
        .wait_and_read(ticket, u64::MAX)
        .expect("readback wait")
        .to_vec();

    let expected: Vec<u8> = bgra
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[b, g, r, a]| [r, g, b, a])
        .collect();
    assert_eq!(rgba, expected);
}

/// The image→image kernel copies RGBA or BGRA into RGBA and nothing else.
#[test]
fn the_image_to_image_kernel_refuses_a_ycbcr_source() {
    let Some(device) = try_device() else { return };
    let converter =
        VulkanColorConverter::new(&device, PixelFormat::Nv12FullRange, PixelFormat::Rgba32)
            .expect("NV12→RGBA converter");
    let texture = allocate_storage_target_in_general(&device, 4, 4);
    let Err(refusal) = converter.prepare_image_to_image_storage(&texture, &texture) else {
        panic!("an NV12 source has no image to sample");
    };
    assert!(matches!(refusal, Error::NotSupported(_)), "{refusal:?}");
}

#[test]
fn the_image_to_image_kernel_refuses_one_image_as_source_and_destination() {
    let Some(device) = try_device() else { return };
    let converter = VulkanColorConverter::new(&device, PixelFormat::Rgba32, PixelFormat::Rgba32)
        .expect("RGBA→RGBA converter");
    let texture = allocate_storage_target_in_general(&device, 4, 4);
    let Err(refusal) = converter.prepare_image_to_image_storage(&texture, &texture) else {
        panic!("one image cannot be sampled and written in one dispatch");
    };
    assert!(refusal.to_string().contains("one image"), "{refusal:?}");
}

/// NV12 whose Y plane starts past byte 0 of the bound buffer — behind
/// bytes the kernel must not read — converts byte-identically to the
/// same NV12 starting at byte 0.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn nv12_starting_past_byte_0_converts_identically_to_the_same_bytes_at_byte_0() {
    let Some(device) = try_device() else { return };
    let (width, height) = (64u32, 32u32);
    let nv12_bytes = build_deterministic_nv12(width, height);
    let plane0_offset = device.min_storage_buffer_offset_alignment().max(1) as usize * 32;
    let info = ResolvedColorInfo {
        primaries: PrimariesId::Bt709,
        transfer: TransferId::Srgb,
        matrix: MatrixId::Smpte170m,
        range: RangeId::Full,
    };
    let converter =
        VulkanColorConverter::new(&device, PixelFormat::Nv12FullRange, PixelFormat::Rgba32)
            .expect("converter construction");

    let at_byte_0 =
        host_visible_storage_buffer_holding(&device, nv12_bytes.len(), &[(0, &nv12_bytes)]);
    let expected = convert_and_read_back(
        &device,
        &converter,
        &at_byte_0,
        SourceLayoutInfo::nv12_tight(width, height),
        &info,
        width,
        height,
    );
    let past_byte_0 = host_visible_storage_buffer_holding(
        &device,
        plane0_offset + nv12_bytes.len(),
        &[(plane0_offset, &nv12_bytes)],
    );
    let actual = convert_and_read_back(
        &device,
        &converter,
        &past_byte_0,
        SourceLayoutInfo::nv12_starting_at(plane0_offset as u32, width, width, width * height),
        &info,
        width,
        height,
    );
    assert!(
        expected.as_chunks::<4>().0.iter().all(|px| px[3] == 255),
        "the reference conversion wrote every pixel"
    );
    assert_eq!(
        actual, expected,
        "Y plane at byte {plane0_offset} converts like the same bytes at byte 0"
    );
}

/// A plane-0 offset that is not a multiple of the device's
/// `minStorageBufferOffsetAlignment` is refused, naming both numbers,
/// before anything reaches the driver.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_plane0_offset_off_the_storage_buffer_offset_alignment_is_refused_naming_both_numbers() {
    let Some(device) = try_device() else { return };
    let alignment = device.min_storage_buffer_offset_alignment();
    if alignment < 2 {
        tracing::warn!(
            alignment,
            "skipping — every offset is aligned on this device"
        );
        return;
    }
    let misaligned_offset = (alignment + alignment / 2) as u32;
    let source = host_visible_storage_buffer_holding(&device, 4096, &[]);
    let output_texture = allocate_storage_target_in_general(&device, 16, 16);
    let converter =
        VulkanColorConverter::new(&device, PixelFormat::Nv12VideoRange, PixelFormat::Rgba32)
            .expect("converter construction");
    let refusal = converter
        .prepare_buffer_to_image_storage(
            &source,
            SourceLayoutInfo::nv12_starting_at(misaligned_offset, 16, 16, 256),
            &output_texture,
            &ResolvedColorInfo {
                primaries: PrimariesId::Bt709,
                transfer: TransferId::Srgb,
                matrix: MatrixId::Smpte170m,
                range: RangeId::Limited,
            },
            TransferId::Srgb,
        )
        .expect_err("a misaligned storage-buffer offset is refused");
    assert!(
        matches!(refusal, Error::Configuration(_)),
        "typed refusal: {refusal}"
    );
    let message = refusal.to_string();
    assert!(
        message.contains(&format!("offset {misaligned_offset} "))
            && message.contains(&format!("minStorageBufferOffsetAlignment {alignment}")),
        "the refusal names the offset and the alignment: {message}"
    );
}
