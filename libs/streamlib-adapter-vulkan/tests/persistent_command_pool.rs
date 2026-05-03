// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_vulkan::tests::persistent_command_pool` — locks
//! the #640 amortisation invariant for `transition_layout_sync`.
//!
//! Before #640 the Vulkan adapter created and destroyed a
//! `vk::CommandPool` on every read / write acquire that needed a
//! layout transition, churning `vkCreateCommandPool` +
//! `vkDestroyCommandPool` on the steady-state per-acquire path. The
//! fix introduced `AdapterPersistentSubmitContext` (same shape as the
//! cpu-readback + cuda adapters from #620) — a single pool + command
//! buffer + completion fence reset and reused across every layout
//! transition. This test locks that invariant: after N>1
//! acquire/release cycles the adapter's `submit_pool_create_count()`
//! must stay at 1.
//!
//! The test alternates read and write acquires so each transition is
//! a real `from != to` swap: read targets `SHADER_READ_ONLY_OPTIMAL`
//! and write targets `GENERAL`, so consecutive acquires of different
//! flavors always trigger a transition (the `from == to` short-
//! circuit doesn't fire).
//!
//! Mentally revert the fix (e.g. force the lazy-init branch on every
//! call) and the assertion fires — that's how this test stays
//! load-bearing rather than feel-good.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use crate::common::HostFixture;

#[test]
fn persistent_pool_count_stays_at_one_across_repeated_acquires() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "vulkan persistent_command_pool: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let surface = fixture.register_surface(1, 16, 16);

    // Pre-condition: lazy-init hasn't fired yet.
    assert_eq!(
        fixture.adapter.submit_pool_create_count(),
        0,
        "adapter should not have created its persistent pool before the first acquire"
    );

    // First acquire materialises the pool exactly once. The first
    // transition is UNDEFINED → SHADER_READ_ONLY_OPTIMAL, which is a
    // real (non-no-op) transition.
    {
        let _r = fixture
            .ctx
            .acquire_read(&surface.descriptor)
            .expect("first acquire_read");
    }
    assert_eq!(
        fixture.adapter.submit_pool_create_count(),
        1,
        "first transition must materialise the persistent pool exactly once"
    );

    // N additional alternating read/write acquires. Each transition is
    // a real swap (SHADER_READ_ONLY_OPTIMAL ↔ GENERAL) so the
    // `from == to` short-circuit doesn't fire — every one drives the
    // persistent pool's record + submit + wait path.
    let cycles = 32usize;
    for i in 0..cycles {
        {
            let _w = fixture
                .ctx
                .acquire_write(&surface.descriptor)
                .unwrap_or_else(|e| panic!("acquire_write cycle {i}: {e:?}"));
        }
        {
            let _r = fixture
                .ctx
                .acquire_read(&surface.descriptor)
                .unwrap_or_else(|e| panic!("acquire_read cycle {i}: {e:?}"));
        }
    }
    assert_eq!(
        fixture.adapter.submit_pool_create_count(),
        1,
        "after {cycles} additional acquire cycles, pool count grew — \
         the persistent pool is being re-created per submit"
    );
}
