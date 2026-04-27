// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::round_trip_write` — customer
//! acquires WRITE, modifies bytes, releases. Host then re-acquires READ
//! and asserts the modifications landed in the host `VkImage`.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use crate::common::HostFixture;

#[test]
fn round_trip_write_persists_modifications() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("round_trip_write: skipping — no Vulkan device available");
            return;
        }
    };

    let width = 24u32;
    let height = 12u32;
    let descriptor = fixture.register_surface(1, width, height);
    let pattern_a: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];
    let pattern_b: [u8; 4] = [0x01, 0x02, 0x03, 0x04];

    // First WRITE round: stamp pattern_a everywhere.
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write 1");
        for chunk in guard
            .view_mut()
            .plane_mut(0)
            .bytes_mut()
            .chunks_exact_mut(4)
        {
            chunk.copy_from_slice(&pattern_a);
        }
    }

    // Second WRITE round: confirm acquire sees pattern_a (the previous
    // release flushed it back), then overwrite with pattern_b.
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write 2");
        for chunk in guard.view().plane(0).bytes().chunks_exact(4) {
            assert_eq!(
                chunk, &pattern_a,
                "second-acquire view should reflect first-release pattern"
            );
        }
        for chunk in guard
            .view_mut()
            .plane_mut(0)
            .bytes_mut()
            .chunks_exact_mut(4)
        {
            chunk.copy_from_slice(&pattern_b);
        }
    }

    // READ round: confirm pattern_b made it.
    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    for chunk in guard.view().plane(0).bytes().chunks_exact(4) {
        assert_eq!(chunk, &pattern_b, "post-write read should observe pattern_b");
    }
}

#[test]
fn round_trip_write_partial_modification_leaves_rest_untouched() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "round_trip_write_partial_modification: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let width = 16u32;
    let height = 8u32;
    let descriptor = fixture.register_surface(2, width, height);
    let base: [u8; 4] = [0x55, 0x55, 0x55, 0xFF];
    let edit: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF];

    // Prime everything to `base`.
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write prime");
        for chunk in guard
            .view_mut()
            .plane_mut(0)
            .bytes_mut()
            .chunks_exact_mut(4)
        {
            chunk.copy_from_slice(&base);
        }
    }

    // Overwrite only row 0.
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write edit");
        let bytes = guard.view_mut().plane_mut(0).bytes_mut();
        let row_bytes = (width as usize) * 4;
        for chunk in bytes[..row_bytes].chunks_exact_mut(4) {
            chunk.copy_from_slice(&edit);
        }
    }

    // Read: row 0 == edit, rows 1..H == base.
    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let bytes = guard.view().plane(0).bytes();
    let row_bytes = (width as usize) * 4;
    for chunk in bytes[..row_bytes].chunks_exact(4) {
        assert_eq!(chunk, &edit, "row 0 should hold edit pattern");
    }
    for chunk in bytes[row_bytes..].chunks_exact(4) {
        assert_eq!(chunk, &base, "row 1..H should still hold base pattern");
    }
}
