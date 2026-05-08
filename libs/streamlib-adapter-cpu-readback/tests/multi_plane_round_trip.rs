// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::multi_plane_round_trip` —
//! the cpu-readback adapter must round-trip multi-plane formats (NV12)
//! plane-by-plane: customer writes a known pattern into each plane,
//! release flushes back, next acquire's read view observes the same
//! pattern in the same plane. Validates:
//!
//!  - Per-plane staging allocation (Y plane and UV plane each get their
//!    own `HostVulkanPixelBuffer`).
//!  - Per-plane `vkCmdCopyImageToBuffer` regions with the right
//!    `VK_IMAGE_ASPECT_PLANE_{0,1}_BIT` aspect — wrong aspect or wrong
//!    extent would scramble the read-back.
//!  - Plane geometry: Y at full resolution, UV at half resolution
//!    (NV12 4:2:0 chroma subsampling).
//!
//! The fresh-allocation NV12 path goes through the host's
//! `acquire_render_target_dma_buf_image(_, _, TextureFormat::Nv12)`
//! which only succeeds when the EGL probe advertised an NV12 modifier
//! that's render-target-capable. Drivers without that modifier (e.g.
//! pre-570 NVIDIA, llvmpipe-only) skip the test cleanly via the
//! fallible registration path.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::{StreamlibSurface, SurfaceFormat};

use crate::common::HostFixture;

/// Distinct, non-trivial bytes per plane so any aspect-mask swap
/// (Y ↔ UV) shows up as a byte-exact mismatch instead of looking
/// "almost right."
const Y_BYTE: u8 = 0x42;
const UV_BYTES: [u8; 2] = [0x80, 0xC0]; // U = 0x80 (neutral), V = 0xC0 (red shift)

/// Try to register an NV12 surface; return `None` (and log) if the
/// driver doesn't support an NV12 render-target DRM modifier.
fn register_nv12_or_skip(
    fixture: &HostFixture,
    id: u64,
    width: u32,
    height: u32,
    test_name: &str,
) -> Option<StreamlibSurface> {
    match fixture.try_register_surface_with_format(
        id,
        width,
        height,
        SurfaceFormat::Nv12,
        TextureFormat::Nv12,
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            println!(
                "{test_name}: skipping — host can't allocate NV12 \
                 render-target DMA-BUF on this driver ({e})"
            );
            None
        }
    }
}

#[test]
fn nv12_multi_plane_write_round_trips_to_read() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("nv12_multi_plane_write_round_trips_to_read: skipping — no Vulkan device available");
            return;
        }
    };

    // NV12 dims must be even (UV plane is half-resolution). Pick a
    // width that's not a power of 2 so plane-1 stride ≠ a friendly
    // alignment — catches a buffer_row_length misuse on the UV plane.
    let width = 36u32;
    let height = 8u32;

    let descriptor = match register_nv12_or_skip(
        &fixture,
        1,
        width,
        height,
        "nv12_multi_plane_write_round_trips_to_read",
    ) {
        Some(d) => d,
        None => return,
    };

    // Surface-level metadata reflects the format.
    assert_eq!(descriptor.format, SurfaceFormat::Nv12);
    assert_eq!(descriptor.width, width);
    assert_eq!(descriptor.height, height);

    // First WRITE round: stamp a known per-plane pattern.
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write 1");

        // Per-plane geometry assertions: Y at full resolution
        // (1 byte per texel), UV at half resolution (2 bytes per
        // texel, interleaved Cb/Cr).
        let view = guard.view();
        assert_eq!(view.format(), SurfaceFormat::Nv12);
        assert_eq!(view.plane_count(), 2);
        assert_eq!(view.width(), width);
        assert_eq!(view.height(), height);

        let y = view.plane(0);
        assert_eq!(y.width(), width);
        assert_eq!(y.height(), height);
        assert_eq!(y.bytes_per_pixel(), 1);
        assert_eq!(y.row_stride(), width);
        assert_eq!(y.bytes().len() as u32, width * height);

        let uv = view.plane(1);
        assert_eq!(uv.width(), width / 2);
        assert_eq!(uv.height(), height / 2);
        assert_eq!(uv.bytes_per_pixel(), 2);
        assert_eq!(uv.row_stride(), (width / 2) * 2);
        assert_eq!(uv.bytes().len() as u32, (width / 2) * (height / 2) * 2);

        // Stamp Y plane uniform.
        for byte in guard.view_mut().plane_mut(0).bytes_mut().iter_mut() {
            *byte = Y_BYTE;
        }
        // Stamp UV plane interleaved [U, V, U, V, …].
        for chunk in guard
            .view_mut()
            .plane_mut(1)
            .bytes_mut()
            .chunks_exact_mut(2)
        {
            chunk.copy_from_slice(&UV_BYTES);
        }
    }

    // READ acquire after release re-runs the GPU→CPU copy — observed
    // bytes must match what we stamped.
    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let view = guard.view();

    let y = view.plane(0);
    let y_bytes = y.bytes();
    assert_eq!(y_bytes.len() as u32, width * height);
    for (i, &b) in y_bytes.iter().enumerate() {
        assert_eq!(b, Y_BYTE, "Y plane byte {i} mismatch: {b:02x}");
    }

    let uv = view.plane(1);
    let uv_bytes = uv.bytes();
    assert_eq!(uv_bytes.len() as u32, (width / 2) * (height / 2) * 2);
    for (i, chunk) in uv_bytes.chunks_exact(2).enumerate() {
        assert_eq!(
            chunk, &UV_BYTES,
            "UV pair {i} mismatch: {chunk:02x?}"
        );
    }
}

#[test]
fn nv12_per_plane_distinct_patterns_lands_unscrambled() {
    // Distinct bytes per row of each plane catch column-vs-row swaps
    // and aspect-mask swaps simultaneously — a test where both planes
    // hold uniform bytes can pass even when Y and UV are swapped.
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("nv12_per_plane_distinct_patterns: skipping — no Vulkan device available");
            return;
        }
    };

    let width = 16u32;
    let height = 8u32;
    let descriptor = match register_nv12_or_skip(
        &fixture,
        2,
        width,
        height,
        "nv12_per_plane_distinct_patterns",
    ) {
        Some(d) => d,
        None => return,
    };

    // Prime: Y row N := byte (N + 0x10), UV row M := pair
    // (M + 0x40, M + 0x80).
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write");
        let view_mut = guard.view_mut();

        // Y plane.
        {
            let y = view_mut.plane_mut(0);
            let yw = y.width() as usize;
            let yh = y.height() as usize;
            let bytes = y.bytes_mut();
            for row in 0..yh {
                let v = row as u8 + 0x10;
                let start = row * yw;
                for byte in &mut bytes[start..start + yw] {
                    *byte = v;
                }
            }
        }

        // UV plane (half resolution, 2 bytes per texel).
        {
            let uv = view_mut.plane_mut(1);
            let uvw = uv.width() as usize;
            let uvh = uv.height() as usize;
            let bytes = uv.bytes_mut();
            for row in 0..uvh {
                let u = row as u8 + 0x40;
                let v = row as u8 + 0x80;
                let row_start = row * uvw * 2;
                for col in 0..uvw {
                    let off = row_start + col * 2;
                    bytes[off] = u;
                    bytes[off + 1] = v;
                }
            }
        }
    }

    // Read back and assert per-row patterns.
    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let view = guard.view();

    let y = view.plane(0);
    let yw = y.width() as usize;
    let yh = y.height() as usize;
    let y_bytes = y.bytes();
    for row in 0..yh {
        let expected = row as u8 + 0x10;
        for col in 0..yw {
            assert_eq!(
                y_bytes[row * yw + col],
                expected,
                "Y plane row {row} col {col} mismatch"
            );
        }
    }

    let uv = view.plane(1);
    let uvw = uv.width() as usize;
    let uvh = uv.height() as usize;
    let uv_bytes = uv.bytes();
    for row in 0..uvh {
        let expected_u = row as u8 + 0x40;
        let expected_v = row as u8 + 0x80;
        for col in 0..uvw {
            let off = row * uvw * 2 + col * 2;
            assert_eq!(
                uv_bytes[off], expected_u,
                "UV plane row {row} col {col} U mismatch"
            );
            assert_eq!(
                uv_bytes[off + 1], expected_v,
                "UV plane row {row} col {col} V mismatch"
            );
        }
    }
}
