// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`SimpleJpegDecoder`] tests — slot rotation, allocation hygiene,
//! typed-error surfaces, end-to-end PSNR round-trip.
//!
//! All GPU-bearing tests skip cleanly when no Vulkan-capable device is
//! present, mirroring `tests/gpu_decode.rs`.

#![cfg(target_os = "linux")]
#![allow(clippy::needless_range_loop)]

use std::sync::Arc;

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::HostTextureExt;
use streamlib::sdk::engine::host_rhi::{HostVulkanDevice, VulkanTextureReadback};
use streamlib::sdk::rhi::{TextureFormat, TextureReadbackDescriptor, TextureSourceLayout};
use streamlib::sdk::color::MatrixId;
use vulkan_jpeg::{JpegColorSource, JpegDecodeOutput, SimpleJpegDecoder, MAX_FRAMES_IN_FLIGHT};

/// Acquire a `GpuContext` for tests, or skip cleanly when no GPU is
/// available (the workstation has one; CI baseline runners may not).
/// Probing `HostVulkanDevice::new` first means we skip on the host side
/// before paying the cost of initializing the full GpuContext.
fn fresh_gpu_context() -> Option<GpuContext> {
    HostVulkanDevice::new().ok()?;
    GpuContext::init_for_platform().ok()
}

fn synthesize_test_image(width: u16, height: u16) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    for y in 0..height {
        for x in 0..width {
            // Smoothly-varying gradients across R/G/B so Y, Cb, Cr all
            // carry signal; matches the gpu_decode.rs fixture math —
            // small high-frequency aliasing terms populate AC
            // coefficients so the pipeline isn't tested only on DC.
            let r = ((u32::from(x) * 4 + u32::from(y) * 3) & 0xFF) as u8;
            let g = ((u32::from(x) * 5 + u32::from(y) * 7 + 32) & 0xFF) as u8;
            let b = ((u32::from(x) * 3 + u32::from(y) * 11 + 96) & 0xFF) as u8;
            rgb.extend_from_slice(&[r, g, b]);
        }
    }
    rgb
}

/// Smoother fixture for round-trip PSNR — clean per-channel gradients
/// with no high-frequency aliasing. JPEG compresses this comfortably
/// at baseline-quality settings (Q=85), unlike the stress fixture
/// above which sits ~33 dB at Q=85 on Y PSNR vs source.
fn synthesize_smooth_test_image(width: u16, height: u16) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    let w = u32::from(width.saturating_sub(1).max(1));
    let h = u32::from(height.saturating_sub(1).max(1));
    for y in 0..height {
        for x in 0..width {
            let r = (u32::from(x) * 255 / w) as u8;
            let g = (u32::from(y) * 255 / h) as u8;
            let b = ((u32::from(x) + u32::from(y)) * 255 / (w + h)) as u8;
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

fn readback_rgba(
    device: &Arc<HostVulkanDevice>,
    output: &JpegDecodeOutput,
) -> Vec<u8> {
    let readback = VulkanTextureReadback::new(
        device,
        &TextureReadbackDescriptor {
            label: "simple_jpeg_test_readback",
            format: TextureFormat::Rgba8Unorm,
            width: output.width,
            height: output.height,
        },
    )
    .expect("readback handle");
    let ticket = readback
        .submit(&output.texture, TextureSourceLayout::General)
        .expect("readback submit");
    readback
        .wait_and_read(ticket, u64::MAX)
        .expect("readback wait")
        .to_vec()
}

// -----------------------------------------------------------------------------
// Construction + rotation
// -----------------------------------------------------------------------------

#[test]
fn new_pre_allocates_ring_at_max_frames_in_flight_depth() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 128, 96))
        .expect("decoder construction");

    assert_eq!(
        decoder.ring().len(),
        MAX_FRAMES_IN_FLIGHT,
        "decoder ring must have MAX_FRAMES_IN_FLIGHT slots"
    );
    assert_eq!(decoder.max_width(), 128);
    assert_eq!(decoder.max_height(), 96);
    assert_eq!(decoder.ring().width(), 128);
    assert_eq!(decoder.ring().height(), 96);
    assert_eq!(decoder.ring().format(), TextureFormat::Rgba8Unorm);
}

#[test]
fn new_eagerly_transitions_ring_slots_to_general_and_updates_registration() {
    use streamlib::sdk::rhi::VulkanLayout;

    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    for slot_index in 0..decoder.ring().len() {
        let slot = decoder.ring().slot(slot_index).expect("ring slot");
        let reg = gpu
            .resolve_texture_registration_by_surface_id(&slot.surface_id, None, 64, 64)
            .expect("registration in cache");
        assert_eq!(
            reg.current_layout(),
            VulkanLayout::GENERAL,
            "slot {slot_index} layout must be GENERAL after SimpleJpegDecoder::new \
             eager transition (anti-pattern #2: registration-vs-reality drift)"
        );
    }
}

#[test]
fn decode_rotates_ring_and_reuses_pre_allocated_slot_textures() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    // Snapshot the underlying Arc<HostVulkanTexture> pointer for each
    // ring slot before any decode runs. After multiple decodes, the
    // ring must hand back the SAME Arcs — pointer drift would mean
    // re-allocation, defeating the steady-state-no-alloc invariant.
    let initial_arcs: Vec<_> = (0..decoder.ring().len())
        .map(|i| {
            Arc::as_ptr(decoder.ring().slot(i).expect("ring slot").texture.vulkan_inner())
        })
        .collect();

    let rgb = synthesize_test_image(48, 48);
    let jpeg = encode_jpeg_rgb_420(48, 48, &rgb, 85);

    let mut surface_ids = Vec::new();
    let mut texture_ptrs = Vec::new();
    for _ in 0..(MAX_FRAMES_IN_FLIGHT * 2) {
        let out = decoder.decode(&jpeg).expect("decode");
        surface_ids.push(out.surface_id.clone());
        texture_ptrs.push(Arc::as_ptr(out.texture.vulkan_inner()));
    }

    // Two passes through a 2-slot ring → surface_ids should match across
    // pass 1 and pass 2.
    assert_eq!(
        surface_ids[0], surface_ids[MAX_FRAMES_IN_FLIGHT],
        "second-pass slot 0 surface_id should match first-pass slot 0 — \
         ring rotation order must be deterministic"
    );
    assert_eq!(
        surface_ids[1],
        surface_ids[MAX_FRAMES_IN_FLIGHT + 1],
        "second-pass slot 1 surface_id should match first-pass slot 1"
    );
    assert_ne!(
        surface_ids[0], surface_ids[1],
        "consecutive decodes within a single pass must hand back distinct slots"
    );

    // Every observed texture ptr must match one of the pre-allocated
    // ring Arcs — i.e. no decode re-allocated a slot texture.
    for (i, ptr) in texture_ptrs.iter().enumerate() {
        assert!(
            initial_arcs.contains(ptr),
            "decode #{i} returned a texture Arc that wasn't pre-allocated by the ring \
             (initial arcs {initial_arcs:?}, got {ptr:?}) — steady-state allocation regression"
        );
    }
}

// -----------------------------------------------------------------------------
// Error surfaces
// -----------------------------------------------------------------------------

#[test]
fn new_rejects_zero_dimensions() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let err = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 0, 64))
        .expect_err("zero max_width must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("max dimensions must be non-zero"),
        "expected typed zero-dim error, got: {msg}"
    );
}

#[test]
fn decode_rejects_oversize_frame_with_typed_error() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 32, 32))
        .expect("decoder construction");

    // 64x64 source exceeds the 32x32 decoder maxima.
    let rgb = synthesize_test_image(64, 64);
    let jpeg = encode_jpeg_rgb_420(64, 64, &rgb, 85);
    let err = decoder.decode(&jpeg).expect_err("oversize must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("exceeds decoder maxima"),
        "expected oversize-typed error, got: {msg}"
    );
}

#[test]
fn decode_rejects_empty_input_with_typed_error() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    let err = decoder.decode(&[]).expect_err("empty input must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("jpeg parse/huffman"),
        "expected wrapped parser error, got: {msg}"
    );
}

#[test]
fn decode_rejects_missing_soi_with_typed_error() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    let err = decoder
        .decode(&[0u8, 1, 2, 3])
        .expect_err("missing SOI must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("jpeg parse/huffman"),
        "expected wrapped parser error, got: {msg}"
    );
}

#[test]
fn decode_rejects_progressive_sof_with_typed_error() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    // Encode a baseline JPEG, then patch SOF0 (0xFF 0xC0) → SOF2 (0xFF
    // 0xC2) to make it progressive — the parser must reject it.
    let rgb = synthesize_test_image(32, 32);
    let mut jpeg = encode_jpeg_rgb_420(32, 32, &rgb, 85);
    let pos = jpeg
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("SOF0 marker present in baseline JPEG");
    jpeg[pos + 1] = 0xC2;

    let err = decoder.decode(&jpeg).expect_err("progressive must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("jpeg parse/huffman"),
        "expected wrapped parser error, got: {msg}"
    );
}

/// APP14 transform=2 (YCCK / 4-component CMYK) must bubble up as a
/// typed `Error::GpuError` from [`SimpleJpegDecoder::decode`] — the
/// 3-component fused kernel cannot handle 4-component CMYK and the
/// resolver returns `JpegError::Unsupported`, which `decode()` wraps
/// as `"jpeg colorimetry: ..."`.
///
/// Locks the bubble-up path. Mentally revert `simple_decoder.rs`'s
/// `decoded.color_info.resolve()?` call (e.g. ignore the error) and
/// this test fails — the decode would then proceed to the kernel
/// dispatch with a JFIF default, masking the unsupported declaration.
#[test]
fn decode_rejects_app14_ycck_with_typed_error() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    // Encode a baseline JPEG, then splice APP14 transform=2 after SOI.
    let rgb = synthesize_test_image(32, 32);
    let baseline = encode_jpeg_rgb_420(32, 32, &rgb, 85);
    let jpeg = splice_app14_after_soi(&baseline, /* transform */ 2);

    let err = decoder.decode(&jpeg).expect_err("YCCK must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("jpeg colorimetry") && msg.contains("YCCK"),
        "expected wrapped colorimetry/YCCK error, got: {msg}"
    );
}

/// APP14 transform=0 (RGB-direct) decode succeeds and surfaces
/// `JpegColorSource::AdobeRgbDirect` + `MatrixId::Identity` on the
/// output handle. Locks the new `JpegDecodeOutput::color_source` /
/// `color_info` fields end-to-end — downstream consumers (e.g. an
/// AGP-style vision pipeline that wants to log "what matrix did
/// we decode under this frame?") rely on this surface.
#[test]
fn decode_app14_transform_zero_surfaces_adobe_rgb_direct_on_output() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    let rgb = synthesize_test_image(32, 32);
    let baseline = encode_jpeg_rgb_420(32, 32, &rgb, 85);
    let jpeg = splice_app14_after_soi(&baseline, /* transform */ 0);

    let output = decoder.decode(&jpeg).expect("APP14 transform=0 decode");
    assert_eq!(output.color_source, JpegColorSource::AdobeRgbDirect);
    assert_eq!(output.color_info.matrix, MatrixId::Identity);
}

/// Splice an APP14 Adobe segment with the given `transform` value
/// directly after the SOI marker of `baseline`. Same shape as the
/// helper in `tests/gpu_decode.rs`; kept here so the two test files
/// stay independent.
fn splice_app14_after_soi(baseline: &[u8], transform: u8) -> Vec<u8> {
    assert!(baseline.len() >= 2 && baseline[0] == 0xFF && baseline[1] == 0xD8);
    let mut payload = Vec::with_capacity(13);
    payload.extend_from_slice(b"Adobe\0");
    payload.extend_from_slice(&[0x00, 0x65]);
    payload.extend_from_slice(&[0x00, 0x00]);
    payload.extend_from_slice(&[0x00, 0x00]);
    payload.push(transform);

    let length = (payload.len() + 2) as u16;
    let mut segment = Vec::with_capacity(4 + payload.len());
    segment.push(0xFF);
    segment.push(0xEE);
    segment.extend_from_slice(&length.to_be_bytes());
    segment.extend_from_slice(&payload);

    let mut out = Vec::with_capacity(baseline.len() + segment.len());
    out.extend_from_slice(&baseline[..2]);
    out.extend_from_slice(&segment);
    out.extend_from_slice(&baseline[2..]);
    out
}

// -----------------------------------------------------------------------------
// End-to-end round-trip + PSNR
// -----------------------------------------------------------------------------

const PSNR_FLOOR_DB_ROUND_TRIP: f64 = 35.0;

/// Compute Y-channel PSNR between a source RGB image and the decoder's
/// RGBA output, both in scanline order. BT.601 luma weights —
/// independent of any production matrix code.
fn y_channel_psnr_db(reference_rgb: &[u8], actual_rgba: &[u8]) -> f64 {
    assert_eq!(reference_rgb.len() % 3, 0, "reference must be RGB-packed");
    assert_eq!(actual_rgba.len() % 4, 0, "actual must be RGBA-packed");
    let pixel_count = reference_rgb.len() / 3;
    assert_eq!(actual_rgba.len() / 4, pixel_count, "pixel counts must match");

    let mut sum_sq_err = 0.0f64;
    for px in 0..pixel_count {
        let r_ref = f64::from(reference_rgb[px * 3]);
        let g_ref = f64::from(reference_rgb[px * 3 + 1]);
        let b_ref = f64::from(reference_rgb[px * 3 + 2]);
        let r_act = f64::from(actual_rgba[px * 4]);
        let g_act = f64::from(actual_rgba[px * 4 + 1]);
        let b_act = f64::from(actual_rgba[px * 4 + 2]);
        let y_ref = 0.299 * r_ref + 0.587 * g_ref + 0.114 * b_ref;
        let y_act = 0.299 * r_act + 0.587 * g_act + 0.114 * b_act;
        let err = y_ref - y_act;
        sum_sq_err += err * err;
    }
    if sum_sq_err == 0.0 {
        return f64::INFINITY;
    }
    let mse = sum_sq_err / pixel_count as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

#[test]
fn end_to_end_round_trip_y_psnr_at_least_35db() {
    const W: u16 = 64;
    const H: u16 = 64;
    // Baseline-quality JPEG per the issue body. The smooth fixture
    // (`synthesize_smooth_test_image`) clears 35 dB comfortably here;
    // the stress fixture used by the other tests in this file
    // populates aggressive AC coefficients on purpose and would sit
    // ~33 dB at this quality, so it's not the right pick for the
    // round-trip gate.
    const QUALITY: u8 = 85;

    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let host_device = HostVulkanDevice::new().ok().map(Arc::new).expect(
        "GpuContext bootstrapped — HostVulkanDevice must be available",
    );

    let rgb = synthesize_smooth_test_image(W, H);
    let jpeg = encode_jpeg_rgb_420(W, H, &rgb, QUALITY);

    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, u32::from(W), u32::from(H)))
        .expect("decoder construction");

    let out = decoder.decode(&jpeg).expect("decode");
    assert_eq!(out.width, u32::from(W));
    assert_eq!(out.height, u32::from(H));

    let rgba = readback_rgba(&host_device, &out);
    let psnr = y_channel_psnr_db(&rgb, &rgba);
    tracing::info!(psnr_db = psnr, floor_db = PSNR_FLOOR_DB_ROUND_TRIP, "Y PSNR");

    // Sanity gate: output isn't all zeros. A fully-black readback would
    // otherwise leak through with PSNR computed against the source —
    // catches the failure mode where the kernel ran but didn't write
    // (e.g. wrong layout, wrong binding, descriptor set unbound).
    assert!(
        rgba.chunks_exact(4).any(|p| p[0] != 0 || p[1] != 0 || p[2] != 0),
        "readback was entirely zeros — kernel did not write the output texture"
    );

    assert!(
        psnr >= PSNR_FLOOR_DB_ROUND_TRIP,
        "round-trip Y PSNR {psnr:.2} dB below floor {PSNR_FLOOR_DB_ROUND_TRIP} dB \
         — JPEG quality 85 should comfortably clear this"
    );
}

/// Locks the contract: parser/Huffman rejection (which aborts before
/// any GPU work runs) must not leave `SimpleJpegDecoder` in a state
/// that breaks subsequent decodes. Doesn't exercise mid-dispatch
/// failure recovery — that's a harder scenario to construct
/// deterministically from test-controllable inputs.
#[test]
fn decode_after_parser_error_state_is_reusable() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64))
        .expect("decoder construction");

    let _ = decoder.decode(&[]).expect_err("empty input rejection");
    let _ = decoder
        .decode(&[0u8, 1, 2])
        .expect_err("garbage input rejection");

    // Good decode after the rejections must still work — verifies the
    // parser-error path doesn't leave the decoder/kernel/ring in a
    // half-baked state.
    let rgb = synthesize_test_image(32, 32);
    let jpeg = encode_jpeg_rgb_420(32, 32, &rgb, 85);
    let out = decoder
        .decode(&jpeg)
        .expect("good decode after prior parser errors must succeed");
    assert_eq!(out.width, 32);
    assert_eq!(out.height, 32);
}
