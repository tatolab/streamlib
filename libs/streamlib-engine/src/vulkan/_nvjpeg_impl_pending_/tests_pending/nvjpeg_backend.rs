// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`NvJpegBackend`] integration tests — exercises the engine-allocates /
//! vendor-imports OPAQUE_FD interop path end-to-end on hosts where the
//! nvJPEG library + NVIDIA CUDA hardware are present. Skips cleanly on
//! every other host (Mesa / non-NVIDIA / no libnvjpeg) so the same
//! test file passes in CI without nvJPEG installed.
//!
//! The Vulkan-compute backend has its own broader regression coverage in
//! `tests/simple_decoder.rs`; this file specifically locks the nvJPEG
//! path so regressions in the CUDA + libnvjpeg integration surface
//! immediately rather than silently re-routing through the fallback.

#![cfg(target_os = "linux")]
#![allow(clippy::needless_range_loop)]

use std::sync::Arc;

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::host_rhi::{HostVulkanDevice, VulkanTextureReadback};
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::rhi::{TextureFormat, TextureReadbackDescriptor, TextureSourceLayout};
use vulkan_jpeg::{
    JpegBackendKind, JpegBackendPreference, JpegDecodeOutput, SimpleJpegDecoder,
};

/// Acquire a `GpuContext` for tests, or skip cleanly when no GPU is
/// available or nvJPEG isn't wired on this host.
fn fresh_nvjpeg_gpu_context() -> Option<GpuContext> {
    // Vulkan device must come up first — gates GpuContext init.
    let device = HostVulkanDevice::new().ok()?;
    if !device.third_party_gpu_capabilities().nvjpeg {
        eprintln!(
            "Skipping nvJPEG test — ThirdPartyGpuCapabilities::nvjpeg=false \
             (non-NVIDIA host or libnvjpeg.so.12 not installed)"
        );
        return None;
    }
    GpuContext::init_for_platform().ok()
}

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

fn readback_rgba(device: &Arc<HostVulkanDevice>, output: &JpegDecodeOutput) -> Vec<u8> {
    use streamlib::sdk::engine::HostTextureExt;
    let readback = VulkanTextureReadback::new(
        device,
        &TextureReadbackDescriptor {
            label: "nvjpeg_test_readback",
            format: TextureFormat::Rgba8Unorm,
            width: output.width,
            height: output.height,
        },
    )
    .expect("readback handle");
    let _ = output.texture.vulkan_inner();
    let ticket = readback
        .submit(&output.texture, TextureSourceLayout::General)
        .expect("readback submit");
    readback
        .wait_and_read(ticket, u64::MAX)
        .expect("readback wait")
        .to_vec()
}

/// Y-channel PSNR against the source RGB image. BT.601 luma weights —
/// independent of any production matrix code; mirrors the helper in
/// `tests/simple_decoder.rs::y_channel_psnr_db`.
fn y_channel_psnr_db(reference_rgb: &[u8], actual_rgba: &[u8]) -> f64 {
    assert_eq!(reference_rgb.len() % 3, 0);
    assert_eq!(actual_rgba.len() % 4, 0);
    let pixel_count = reference_rgb.len() / 3;
    assert_eq!(actual_rgba.len() / 4, pixel_count);
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
    let mse = sum_sq_err / pixel_count as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0_f64.powi(2) / mse).log10()
    }
}

const PSNR_FLOOR_DB: f64 = 35.0;

/// Decode-and-readback round-trip with the nvJPEG backend explicitly
/// selected via [`JpegBackendPreference::Force`]. Locks the OPAQUE_FD
/// staging path end-to-end: `nvjpegDecode` writes RGBI into the CUDA-
/// private buffer, `cudaMemcpy2DAsync` repitches into the shared
/// OPAQUE_FD buffer (with alpha pre-fill leaving the 4th byte at 0xFF),
/// the Vulkan-side `vkCmdCopyBufferToImage` lands it in the ring slot,
/// and the readback returns an Rgba8Unorm scanline.
///
/// Y-channel PSNR vs the source RGB must clear the 35 dB floor —
/// matches the cross-vendor backend's PSNR test, so the two backends
/// are quality-comparable on baseline 4:2:0 input.
#[test]
fn nvjpeg_backend_round_trip_y_psnr_at_least_35db() {
    let Some(gpu) = fresh_nvjpeg_gpu_context() else {
        return;
    };
    let device = gpu.device().vulkan_device().clone();

    let mut decoder = gpu
        .limited_access()
        .escalate(|full| {
            SimpleJpegDecoder::new_with_preference(
                full,
                64,
                64,
                JpegBackendPreference::Force(JpegBackendKind::NvJpeg),
            )
        })
        .expect("nvJPEG decoder construction");
    assert_eq!(
        decoder.backend_kind(),
        JpegBackendKind::NvJpeg,
        "Force(NvJpeg) must select the nvJPEG backend"
    );

    let rgb = synthesize_smooth_test_image(64, 64);
    let jpeg = encode_jpeg_rgb_420(64, 64, &rgb, 92);
    let output = decoder.decode(&jpeg).expect("nvJPEG decode");
    assert_eq!(output.width, 64);
    assert_eq!(output.height, 64);

    let readback = readback_rgba(&device, &output);

    // Alpha channel: every pixel must be 0xFF (the cudaMemset pre-fill,
    // preserved across cudaMemcpy2DAsync's stride trick). A regression
    // in the alpha-padding path would surface as alpha != 255 here.
    for px in 0..(64 * 64) {
        assert_eq!(
            readback[px * 4 + 3],
            0xFF,
            "pixel {px} alpha must be 0xFF (alpha pre-fill regression)"
        );
    }

    let psnr = y_channel_psnr_db(&rgb, &readback);
    assert!(
        psnr >= PSNR_FLOOR_DB,
        "nvJPEG Y-channel PSNR {psnr:.2} dB < {PSNR_FLOOR_DB} dB floor — \
         color-correctness regression in the nvJPEG → OPAQUE_FD → Vulkan path"
    );
}

/// Two consecutive decodes through the 2-slot ring must hand back
/// distinct `surface_id`s, then re-use them on the third + fourth
/// decodes. Locks the ring rotation through the nvJPEG path — separate
/// from the Vulkan-compute backend's equivalent test in
/// `tests/simple_decoder.rs`.
#[test]
fn nvjpeg_backend_decode_rotates_ring_deterministically() {
    let Some(gpu) = fresh_nvjpeg_gpu_context() else {
        return;
    };
    let mut decoder = gpu
        .limited_access()
        .escalate(|full| {
            SimpleJpegDecoder::new_with_preference(
                full,
                64,
                64,
                JpegBackendPreference::Force(JpegBackendKind::NvJpeg),
            )
        })
        .expect("nvJPEG decoder construction");

    let rgb = synthesize_smooth_test_image(48, 48);
    let jpeg = encode_jpeg_rgb_420(48, 48, &rgb, 85);

    let mut surface_ids = Vec::new();
    for _ in 0..4 {
        let out = decoder.decode(&jpeg).expect("decode");
        surface_ids.push(out.surface_id.clone());
    }

    assert_ne!(
        surface_ids[0], surface_ids[1],
        "consecutive decodes must hand back distinct ring slots"
    );
    assert_eq!(
        surface_ids[0], surface_ids[2],
        "third decode must reuse the first slot (2-slot ring)"
    );
    assert_eq!(
        surface_ids[1], surface_ids[3],
        "fourth decode must reuse the second slot (2-slot ring)"
    );
}

/// [`JpegBackendPreference::Force(NvJpeg)`] on a non-NVIDIA host (or one
/// without libnvjpeg) must surface as a typed
/// [`streamlib::sdk::error::Error::NotSupported`] from `new_with_preference`
/// rather than panicking, transparently falling back, or silently
/// using Vulkan-compute. This is the only "negative" capability test
/// that runs on every host — the rest skip when nvjpeg is available.
///
/// The shape of the test: query the capability struct, then try to
/// force nvJPEG. If the capability says `false`, the construction must
/// return `NotSupported`. Locks the gating logic in
/// `simple_decoder::build_backend`.
#[test]
fn force_nvjpeg_returns_not_supported_when_capability_false() {
    let Some(device) = HostVulkanDevice::new().ok() else {
        eprintln!("Skipping — no Vulkan device available");
        return;
    };
    if device.third_party_gpu_capabilities().nvjpeg {
        eprintln!(
            "Skipping — nvJPEG IS available on this host; the negative path \
             can't fire here. Run on a Mesa or no-CUDA host to exercise it."
        );
        return;
    }
    let Some(gpu) = GpuContext::init_for_platform().ok() else {
        eprintln!("Skipping — GpuContext init failed");
        return;
    };

    let err = gpu
        .limited_access()
        .escalate(|full| {
            SimpleJpegDecoder::new_with_preference(
                full,
                64,
                64,
                JpegBackendPreference::Force(JpegBackendKind::NvJpeg),
            )
        })
        .expect_err("Force(NvJpeg) on a host without nvJPEG must error");

    let msg = format!("{err}");
    assert!(
        msg.contains("nvJPEG not available"),
        "expected typed 'nvJPEG not available' error, got: {msg}"
    );
}
