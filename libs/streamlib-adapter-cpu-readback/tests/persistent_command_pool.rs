// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::persistent_command_pool` —
//! locks the #620 amortisation invariant.
//!
//! Before #620 the in-process trigger created and destroyed a
//! `vk::CommandPool` on every read / write release, churning
//! `vkCreateCommandPool` + `vkDestroyCommandPool` once per acquire on
//! the steady-state hot path. The fix introduced
//! `AdapterPersistentSubmitContext` — a single pool + command buffer +
//! completion fence reset and reused across every submit. This test
//! locks that invariant: after N>1 acquire/release cycles the
//! in-process trigger's `submit_pool_create_count()` must stay at 1.
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
                "persistent_command_pool: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let descriptor = fixture.register_surface(1, 16, 16);

    // Pre-condition: lazy-init hasn't fired yet.
    assert_eq!(
        fixture.trigger.submit_pool_create_count(),
        0,
        "trigger should not have created its persistent pool before the first acquire"
    );

    // First acquire materialises the pool exactly once.
    {
        let _guard = fixture
            .ctx
            .acquire_read(&descriptor)
            .expect("first acquire_read");
    }
    assert_eq!(
        fixture.trigger.submit_pool_create_count(),
        1,
        "first submit must materialise the persistent pool exactly once"
    );

    // N additional acquires + write-release cycles must not grow the
    // live pool count. Each `acquire_read` triggers an image→buffer
    // submit; each `acquire_write` + drop triggers a buffer→image
    // submit. Both go through the persistent pool — neither should
    // re-materialise it.
    let cycles = 32usize;
    for i in 0..cycles {
        {
            let _r = fixture
                .ctx
                .acquire_read(&descriptor)
                .unwrap_or_else(|e| panic!("acquire_read cycle {i}: {e:?}"));
        }
        {
            let _w = fixture
                .ctx
                .acquire_write(&descriptor)
                .unwrap_or_else(|e| panic!("acquire_write cycle {i}: {e:?}"));
        }
    }
    assert_eq!(
        fixture.trigger.submit_pool_create_count(),
        1,
        "after {cycles} additional acquire cycles, pool count grew — \
         the persistent pool is being re-created per submit"
    );
}
