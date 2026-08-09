// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Crash semantics for the cpu-readback adapter.
//!
//! Post-Path-E (#562) the cpu-readback adapter spans the same
//! cross-process FD-passing boundary as the Vulkan / OpenGL adapters:
//! the host pre-allocates the source `VkImage`, the per-plane staging
//! `VkBuffer`s, and the timeline; the subprocess imports them via the
//! consumer-rhi carve-out and holds its own
//! `CpuReadbackSurfaceAdapter<ConsumerVulkanDevice>` guard while the
//! customer touches the mapped bytes. A subprocess crash mid-acquire
//! must therefore not perturb the host adapter's per-surface state.
//!
//! `panic_mid_write_releases_lock_for_next_acquire`: a host-thread
//! panic during a write must still run the `WriteGuard`'s `Drop` so
//! the per-surface state releases and the next `acquire_*` succeeds —
//! the same RAII coverage the Vulkan host-side crash test gives.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;



#[test]
fn panic_mid_write_releases_lock_for_next_acquire() {
    let fixture = match common::HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("panic_mid_write: skipping — no Vulkan device available");
            return;
        }
    };

    let descriptor = fixture.register_surface(1, 32, 32);

    // Customer code panics holding a WriteGuard. RAII unwind must run
    // `Drop` and release the per-surface lock.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write before panic");
        // Touch the bytes so the write isn't optimized out.
        guard.view_mut().plane_mut(0).bytes_mut()[0] = 0xAB;
        panic!("simulated customer panic mid-write");
    }));
    assert!(result.is_err(), "the closure must have panicked");

    // Post-panic: the next acquire must succeed (lock released).
    {
        let guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("post-panic acquire_write must succeed");
        assert_eq!(guard.view().plane(0).bytes().len(), 32 * 32 * 4);
    }
    {
        let _g = fixture
            .ctx
            .acquire_read(&descriptor)
            .expect("post-panic acquire_read must succeed");
    }
}
