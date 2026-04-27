// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::stride_offset_handling` —
//! the cpu-readback adapter always hands the customer a tightly-packed
//! byte slice (`width * bytes_per_pixel` per row, no padding). The
//! host's `VkImage` may use a non-tightly-packed DRM modifier internally,
//! but the staging buffer is owned by the adapter and constructed
//! tightly packed. The customer never sees a non-tight stride.
//!
//! These tests document and lock that invariant.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use crate::common::HostFixture;

#[test]
fn stride_is_tightly_packed_width_times_bpp() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "stride_is_tightly_packed: skipping — no Vulkan device available"
            );
            return;
        }
    };

    // Pick a width that is NOT a power of 2 — common driver stride-
    // alignment requirements (256-byte rows, 64-pixel rows on NVIDIA)
    // would surface here if the staging buffer accidentally inherited
    // them.
    let width = 37u32;
    let height = 5u32;
    let bpp = 4u32;

    let descriptor = fixture.register_surface(1, width, height);
    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let view = guard.view();

    assert_eq!(view.width(), width);
    assert_eq!(view.height(), height);
    assert_eq!(view.bytes_per_pixel(), bpp);
    // Adapter's contract: row stride is exactly width * bpp.
    assert_eq!(view.row_stride(), width * bpp);
    // Total slice length is height * row_stride.
    assert_eq!(
        view.bytes().len() as u32,
        height * view.row_stride(),
        "tightly-packed contract: bytes.len() == height * row_stride"
    );
}

#[test]
fn unaligned_widths_round_trip_byte_exact() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "unaligned_widths_round_trip_byte_exact: skipping — no Vulkan device available"
            );
            return;
        }
    };

    // Width = 17 (prime, not aligned to 4/16/64). If the
    // image-to-buffer copy is using the wrong row pitch on the buffer
    // side, every other row will be shifted by the alignment delta and
    // this byte-exact comparison will diverge.
    let width = 17u32;
    let height = 9u32;
    let descriptor = fixture.register_surface(1, width, height);

    // Prime: each pixel's value encodes (y * width + x) mod 256 in all
    // four channels — distinct bytes per row AND per column, so any
    // stride scrambling reorders them visibly.
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write prime");
        let bytes = guard.view_mut().bytes_mut();
        for y in 0..height as usize {
            for x in 0..width as usize {
                let v = ((y * width as usize + x) & 0xFF) as u8;
                let idx = (y * width as usize + x) * 4;
                bytes[idx..idx + 4].copy_from_slice(&[v, v.wrapping_add(1), v.wrapping_add(2), v.wrapping_add(3)]);
            }
        }
    }

    // Read and assert byte-exact match.
    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let bytes = guard.view().bytes();
    for y in 0..height as usize {
        for x in 0..width as usize {
            let v = ((y * width as usize + x) & 0xFF) as u8;
            let idx = (y * width as usize + x) * 4;
            assert_eq!(
                &bytes[idx..idx + 4],
                &[v, v.wrapping_add(1), v.wrapping_add(2), v.wrapping_add(3)],
                "stride scramble at ({x}, {y})"
            );
        }
    }
}
