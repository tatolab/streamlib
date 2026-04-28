// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end check that the cpu-readback adapter's `try_acquire_*`
//! returns `Ok(None)` when another holder is keeping the same surface
//! occupied, and resumes returning `Ok(Some(_))` once that holder drops.
//!
//! Post-#562 (Path E single-pattern shape) contention enforcement
//! lives in the adapter's per-surface `Registry`, not in the
//! cross-process bridge. Each subprocess holds its own
//! `CpuReadbackSurfaceAdapter` with its own registry; the host bridge
//! is a thin trigger. This test exercises the registry directly via
//! the host-flavor adapter — the same code path runs on the consumer
//! side with `ConsumerVulkanDevice`.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::SurfaceAdapter;

use crate::common::HostFixture;

const SURFACE_ID_A: u64 = 0x_544A_0001;
const SURFACE_ID_B: u64 = 0x_544A_0002;

#[test]
fn try_acquire_returns_none_when_write_held() {
    let Some(fixture) = HostFixture::try_new() else {
        println!("try_acquire_returns_none_when_write_held: no GPU — skipping");
        return;
    };
    let surface = fixture.register_surface(SURFACE_ID_A, 16, 16);

    let first = fixture
        .adapter
        .acquire_write(&surface)
        .expect("first acquire_write must succeed against an unheld surface");

    let second = fixture
        .adapter
        .try_acquire_write(&surface)
        .expect("try_acquire_write must not surface contention as an error");
    assert!(
        second.is_none(),
        "try_acquire_write must return Ok(None) while a write guard is held"
    );

    let read_attempt = fixture
        .adapter
        .try_acquire_read(&surface)
        .expect("try_acquire_read must not surface contention as an error");
    assert!(
        read_attempt.is_none(),
        "try_acquire_read must return Ok(None) while a write guard is held"
    );

    drop(first);

    let third = fixture
        .adapter
        .try_acquire_write(&surface)
        .expect("try_acquire_write must not error after contention clears");
    assert!(
        third.is_some(),
        "try_acquire_write must return Ok(Some(_)) once the prior holder drops"
    );
    drop(third);
}

/// Multiple concurrent readers do NOT contend with each other.
#[test]
fn try_acquire_read_not_contended_by_concurrent_readers() {
    let Some(fixture) = HostFixture::try_new() else {
        println!(
            "try_acquire_read_not_contended_by_concurrent_readers: no GPU — skipping"
        );
        return;
    };
    let surface = fixture.register_surface(SURFACE_ID_B, 16, 16);

    let r1 = fixture
        .adapter
        .try_acquire_read(&surface)
        .expect("first try_acquire_read on unheld surface")
        .expect("must succeed when no writer holds the surface");
    let r2 = fixture
        .adapter
        .try_acquire_read(&surface)
        .expect("second try_acquire_read while another reader holds")
        .expect("multiple concurrent readers must coexist");
    drop(r1);
    drop(r2);

    let w = fixture
        .adapter
        .try_acquire_write(&surface)
        .expect("try_acquire_write after readers release")
        .expect("write must succeed once read holders are gone");
    drop(w);
}
