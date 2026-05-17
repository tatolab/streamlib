// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GPU JPEG decode kernel: PSNR regression against an independently-
//! authored CPU reference.
//!
//! Encodes a deterministic RGBA test image to JPEG via `jpeg-encoder`
//! (4:2:0, BT.601 full-range, JFIF), decodes via the crate's CPU
//! parser + Huffman entropy decoder, runs the same dequant + IDCT +
//! chroma-upsample + YCbCr->RGB pipeline on both CPU and GPU sides,
//! and asserts the Y-channel PSNR between the two RGB outputs is at
//! least 50 dB.
//!
//! 50 dB is the precision-not-lossy floor: both sides are decoding
//! the same lossy bitstream, so the only divergence allowed is
//! floating-point order-of-operations in the IDCT + u8 rounding. The
//! CPU reference uses BT.601-full constants hardcoded in this file
//! (independent of any production matrix code) so a regression in
//! either side fails the test loudly.
//!
//! Skipped cleanly when no Vulkan-capable GPU is present (e.g. CI
//! without GPU passthrough).

#![cfg(target_os = "linux")]
#![allow(clippy::needless_range_loop)]

use std::sync::Arc;

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use streamlib::sdk::engine::HostTextureExt;
use streamlib::sdk::engine::host_rhi::{
    HostVulkanDevice, HostVulkanTexture, RhiCommandRecorder, VulkanAccess, VulkanStage,
    VulkanTextureReadback,
};
use streamlib::sdk::rhi::{
    Texture, TextureDescriptor, TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
    TextureUsages, VulkanLayout,
};
use vulkan_jpeg::{decode, ComponentScan, DecodedJpeg, JpegDecodeKernel, QuantizationTable, ZIGZAG};

const QUALITY: u8 = 85;
const TEST_WIDTH: u16 = 64;
const TEST_HEIGHT: u16 = 64;

/// 50 dB target per the issue's Tests/validation section — sets the
/// floor on "GPU and CPU agree to floating-point precision, not on
/// reconstruction of the original image" (which is JPEG-lossy and
/// would never reach 50 dB at quality 85).
const PSNR_FLOOR_DB: f64 = 50.0;

#[test]
fn gpu_decode_matches_cpu_reference_psnr_50db() {
    // Probe for a Vulkan device. Tests that need a real GPU bail
    // cleanly on hosts without one (CI baseline runners, etc.).
    let Some(device) = HostVulkanDevice::new().ok().map(Arc::new) else {
        return;
    };

    // 1. Synthesize a deterministic RGB source image whose color/luma
    //    varies across the frame so chroma actually contributes.
    let rgb = synthesize_test_image(TEST_WIDTH, TEST_HEIGHT);

    // 2. Encode -> JPEG (4:2:0, JFIF / BT.601 full-range).
    let jpeg_bytes = encode_jpeg_rgb_420(TEST_WIDTH, TEST_HEIGHT, &rgb, QUALITY);

    // 3. CPU-side parse + Huffman entropy decode -> coefficient buffers.
    let decoded = decode(&jpeg_bytes).expect("parse + huffman decode");
    assert_eq!(decoded.frame.width, TEST_WIDTH);
    assert_eq!(decoded.frame.height, TEST_HEIGHT);
    assert_eq!(decoded.frame.components.len(), 3, "expected YCbCr");

    // 4. CPU reference: dequant + IDCT + chroma upsample + YCbCr->RGB.
    let cpu_rgba = cpu_reference_decode(&decoded);

    // 5. GPU path: allocate rgba8 storage texture, transition to GENERAL,
    //    build the kernel, dispatch, read back.
    let texture = allocate_storage_texture_general(
        &device,
        u32::from(TEST_WIDTH),
        u32::from(TEST_HEIGHT),
    );
    let kernel = JpegDecodeKernel::new(&device).expect("kernel construction");
    kernel
        .dispatch(&decoded, &texture)
        .expect("kernel dispatch");
    let gpu_rgba = readback_texture(&device, &texture, TEST_WIDTH, TEST_HEIGHT);

    assert_eq!(cpu_rgba.len(), gpu_rgba.len());

    // 6. Compute Y-channel PSNR between CPU and GPU outputs.
    let psnr = y_channel_psnr_db(&cpu_rgba, &gpu_rgba);
    tracing::info!(psnr_db = psnr, floor_db = PSNR_FLOOR_DB, "GPU vs CPU Y-channel PSNR");
    assert!(
        psnr >= PSNR_FLOOR_DB,
        "Y-channel PSNR {psnr:.2} dB fell below floor {PSNR_FLOOR_DB} dB \
         — GPU shader and CPU reference have diverged beyond float / u8 rounding"
    );
}

// ---------------------------------------------------------------------------
// Test-input synthesis
// ---------------------------------------------------------------------------

fn synthesize_test_image(width: u16, height: u16) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    for y in 0..height {
        for x in 0..width {
            // Smoothly varying gradients on each channel so Y, Cb, and Cr
            // all carry signal across the frame. Mix in a small high-
            // frequency term so AC coefficients are populated, not just DC.
            let r = ((u32::from(x) * 4 + u32::from(y) * 3) & 0xFF) as u8;
            let g = ((u32::from(x) * 5 + u32::from(y) * 7 + 32) & 0xFF) as u8;
            let b = ((u32::from(x) * 3 + u32::from(y) * 11 + 96) & 0xFF) as u8;
            rgb.extend_from_slice(&[r, g, b]);
        }
    }
    rgb
}

fn encode_jpeg_rgb_420(width: u16, height: u16, rgb: &[u8], quality: u8) -> Vec<u8> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut encoder = Encoder::new(&mut bytes, quality);
    encoder.set_sampling_factor(SamplingFactor::R_4_2_0);
    encoder
        .encode(rgb, width, height, ColorType::Rgb)
        .expect("jpeg encode");
    bytes
}

// ---------------------------------------------------------------------------
// CPU reference decode: dequant + IDCT + 4:2:0 upsample + YCbCr->RGB
// ---------------------------------------------------------------------------

/// Scan-order positions for a JFIF-compliant 3-component YCbCr stream
/// (Y first, Cb second, Cr third). jpeg-encoder assigns numeric
/// `component_id` values 0/1/2 rather than the JFIF-canonical 1/2/3,
/// and we don't want to depend on either — positional lookup works
/// across every JFIF encoder.
const Y_POSITION: usize = 0;
const CB_POSITION: usize = 1;
const CR_POSITION: usize = 2;

/// CPU reference. Output is rgba8 in scanline order (matches the GPU
/// readback layout for trivial pixel-by-pixel comparison).
fn cpu_reference_decode(decoded: &DecodedJpeg) -> Vec<u8> {
    let width = usize::from(decoded.frame.width);
    let height = usize::from(decoded.frame.height);
    let mut rgba = vec![0u8; width * height * 4];

    let y_plane = &decoded.components[Y_POSITION];
    let cb_plane = &decoded.components[CB_POSITION];
    let cr_plane = &decoded.components[CR_POSITION];

    let y_qt_id = y_plane.quant_table_id;
    let chroma_qt_id = cb_plane.quant_table_id;
    let y_qt = decoded
        .quantization_table(y_qt_id)
        .expect("Y quant table");
    let chroma_qt = decoded
        .quantization_table(chroma_qt_id)
        .expect("chroma quant table");

    for py in 0..height {
        for px in 0..width {
            // Y plane: 8x8 block.
            let y_sample = idct_sample(y_plane, y_qt, px / 8, py / 8, px % 8, py % 8) + 128.0;
            // 4:2:0 chroma: one 8x8 block per 16x16 pixel region, nearest
            // upsample matches the shader exactly.
            let chroma_block_x = px / 16;
            let chroma_block_y = py / 16;
            let chroma_in_x = (px % 16) / 2;
            let chroma_in_y = (py % 16) / 2;
            let cb_sample = idct_sample(
                cb_plane,
                chroma_qt,
                chroma_block_x,
                chroma_block_y,
                chroma_in_x,
                chroma_in_y,
            ) + 128.0;
            let cr_sample = idct_sample(
                cr_plane,
                chroma_qt,
                chroma_block_x,
                chroma_block_y,
                chroma_in_x,
                chroma_in_y,
            ) + 128.0;

            let cb_centered = cb_sample - 128.0;
            let cr_centered = cr_sample - 128.0;
            // BT.601 full-range coefficients hardcoded — must match the
            // shader's hardcoded values verbatim.
            let r = y_sample + 1.402 * cr_centered;
            let g = y_sample - 0.344136 * cb_centered - 0.714136 * cr_centered;
            let b = y_sample + 1.772 * cb_centered;

            let off = (py * width + px) * 4;
            rgba[off] = clamp_to_u8(r);
            rgba[off + 1] = clamp_to_u8(g);
            rgba[off + 2] = clamp_to_u8(b);
            rgba[off + 3] = 255;
        }
    }
    rgba
}

/// Dequantize + 2D IDCT for a single sample at (in_x, in_y) within the
/// 8x8 block at (block_x, block_y). Mirrors the shader's
/// `idct_sample()` math exactly, including the zig-zag iteration.
fn idct_sample(
    plane: &ComponentScan,
    qt: &QuantizationTable,
    block_x: usize,
    block_y: usize,
    in_x: usize,
    in_y: usize,
) -> f32 {
    let block = plane.block(block_x, block_y);
    let mut sum = 0.0f32;
    for zz in 0..64usize {
        let natural = ZIGZAG[zz];
        let u = natural & 7;
        let v = natural >> 3;
        let dequant = f32::from(block[zz]) * f32::from(qt.values[zz]);
        let cu = if u == 0 { core::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
        let cv = if v == 0 { core::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
        let cx = (((2 * in_x + 1) as f32) * (u as f32) * core::f32::consts::PI / 16.0).cos();
        let cy = (((2 * in_y + 1) as f32) * (v as f32) * core::f32::consts::PI / 16.0).cos();
        sum += cu * cv * dequant * cx * cy;
    }
    sum * 0.25
}

fn clamp_to_u8(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

// ---------------------------------------------------------------------------
// GPU side: storage texture in GENERAL, kernel dispatch, readback
// ---------------------------------------------------------------------------

fn allocate_storage_texture_general(
    device: &Arc<HostVulkanDevice>,
    width: u32,
    height: u32,
) -> Texture {
    let descriptor = TextureDescriptor {
        width,
        height,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::COPY_SRC
            | TextureUsages::COPY_DST
            | TextureUsages::STORAGE_BINDING,
        label: Some("jpeg-decode-test-output"),
    };
    let host = HostVulkanTexture::new(device, &descriptor).expect("texture allocation");
    let texture = Texture::from_vulkan(host);

    // Transition UNDEFINED -> GENERAL so the kernel's `imageStore` is
    // spec-legal. `record_image_barrier` handles the empty-slice cast
    // gotcha internally per the vulkanalia-empty-slice-cast learning.
    let mut recorder = RhiCommandRecorder::new(device, "jpeg-decode-test-prep")
        .expect("recorder construction");
    recorder.begin().expect("recorder begin");
    recorder
        .record_image_barrier(
            &texture,
            VulkanLayout::UNDEFINED,
            VulkanLayout::GENERAL,
            VulkanStage::TOP_OF_PIPE,
            VulkanStage::COMPUTE_SHADER,
            VulkanAccess::NONE,
            VulkanAccess::SHADER_WRITE,
        )
        .expect("transition to GENERAL");
    recorder.submit_and_wait().expect("transition submit+wait");
    texture
}

fn readback_texture(
    device: &Arc<HostVulkanDevice>,
    texture: &Texture,
    width: u16,
    height: u16,
) -> Vec<u8> {
    let readback = VulkanTextureReadback::new(
        device,
        &TextureReadbackDescriptor {
            label: "jpeg-decode-test-readback",
            format: TextureFormat::Rgba8Unorm,
            width: u32::from(width),
            height: u32::from(height),
        },
    )
    .expect("readback handle");
    let ticket = readback
        .submit(texture, TextureSourceLayout::General)
        .expect("readback submit");
    readback
        .wait_and_read(ticket, u64::MAX)
        .expect("readback wait")
        .to_vec()
}

// ---------------------------------------------------------------------------
// PSNR
// ---------------------------------------------------------------------------

fn y_channel_psnr_db(reference: &[u8], actual: &[u8]) -> f64 {
    assert_eq!(reference.len(), actual.len());
    assert_eq!(reference.len() % 4, 0, "expected RGBA stride");
    let mut sum_sq_err = 0.0f64;
    let mut count = 0u64;
    for px in 0..(reference.len() / 4) {
        let r_ref = f64::from(reference[px * 4]);
        let g_ref = f64::from(reference[px * 4 + 1]);
        let b_ref = f64::from(reference[px * 4 + 2]);
        let r_act = f64::from(actual[px * 4]);
        let g_act = f64::from(actual[px * 4 + 1]);
        let b_act = f64::from(actual[px * 4 + 2]);
        // BT.601 luma weights — independent of the matrix code under test.
        let y_ref = 0.299 * r_ref + 0.587 * g_ref + 0.114 * b_ref;
        let y_act = 0.299 * r_act + 0.587 * g_act + 0.114 * b_act;
        let err = y_ref - y_act;
        sum_sq_err += err * err;
        count += 1;
    }
    if sum_sq_err == 0.0 {
        return f64::INFINITY;
    }
    let mse = sum_sq_err / count as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}
