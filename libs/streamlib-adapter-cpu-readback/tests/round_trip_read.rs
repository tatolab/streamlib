// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::round_trip_read` — host
//! writes a known pattern into a `VkImage`, customer acquires READ,
//! asserts the bytes the customer sees match the host's pattern.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::CpuReadable;

use crate::common::HostFixture;

/// Helper: prime the host `VkImage` with a known pattern by acquiring
/// a WRITE guard, filling its bytes, and dropping. The WRITE release
/// path flushes those bytes back into the host `VkImage`. The next
/// READ acquire then re-runs the GPU→CPU copy and surfaces them to
/// the customer — that's what we assert.
fn prime_with_pattern(
    fixture: &HostFixture,
    descriptor: &streamlib_adapter_abi::StreamlibSurface,
    pattern: [u8; 4],
) {
    let mut guard = fixture
        .ctx
        .acquire_write(descriptor)
        .expect("prime: acquire_write");
    let bytes = guard.view_mut().bytes_mut();
    for chunk in bytes.chunks_exact_mut(4) {
        chunk.copy_from_slice(&pattern);
    }
}

#[test]
fn round_trip_read_observes_host_pattern() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("round_trip_read: skipping — no Vulkan device available");
            return;
        }
    };

    let descriptor = fixture.register_surface(1, 32, 16);
    let pattern: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
    prime_with_pattern(&fixture, &descriptor, pattern);

    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let view = guard.view();
    assert_eq!(view.width(), 32);
    assert_eq!(view.height(), 16);
    assert_eq!(view.bytes_per_pixel(), 4);
    assert_eq!(view.row_stride(), 32 * 4);
    assert_eq!(view.bytes().len(), 32 * 16 * 4);
    assert_eq!(view.read_bytes().len(), 32 * 16 * 4);

    for (i, chunk) in view.bytes().chunks_exact(4).enumerate() {
        assert_eq!(chunk, &pattern, "pixel {i} mismatch: {chunk:02x?}");
    }
}

#[test]
fn round_trip_read_per_row_pattern_lands_unscrambled() {
    // Distinct pattern per row catches column-vs-row confusion that a
    // uniform pattern would miss.
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "round_trip_read_per_row_pattern: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let width = 16u32;
    let height = 8u32;
    let descriptor = fixture.register_surface(2, width, height);

    // Prime: row N full of bytes [N+0x10, …].
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write");
        let bytes = guard.view_mut().bytes_mut();
        for y in 0..height as usize {
            for x in 0..width as usize {
                let byte = y as u8 + 0x10;
                let idx = (y * width as usize + x) * 4;
                bytes[idx..idx + 4].copy_from_slice(&[byte, byte, byte, byte]);
            }
        }
    }

    // Read back and assert row N is full of [byte, …].
    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let view = guard.view();
    let bytes = view.bytes();
    for y in 0..height as usize {
        for x in 0..width as usize {
            let expected = y as u8 + 0x10;
            let idx = (y * width as usize + x) * 4;
            assert_eq!(
                &bytes[idx..idx + 4],
                &[expected, expected, expected, expected],
                "row {y} col {x} mismatch"
            );
        }
    }
}
