// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::try_acquire_contention` —
//! end-to-end check that the [`CpuReadbackBridge::try_acquire`] path
//! returns `Ok(None)` when another holder is keeping the same surface
//! occupied, and resumes returning `Ok(Some(_))` once that holder drops.
//!
//! This is the host-side stand-in for the polyglot E2E listed in #544's
//! exit criteria: "two subprocess processors race for the same surface;
//! the second's `try_acquire_write` returns the contended path cleanly,
//! no host-side registry leak." Exercising the bridge directly covers
//! the bug surface — wire-format glue is unit-tested in
//! `subprocess_escalate.rs::try_acquire_dispatch` and the polyglot SDK
//! test suites.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::Arc;

use streamlib::core::context::{CpuReadbackAccessMode, CpuReadbackBridge};
use streamlib_adapter_cpu_readback::{CpuReadbackBridgeImpl, CpuReadbackSurfaceAdapter};

use crate::common::HostFixture;

/// Surface ids for the two concurrent acquires. Distinct from any other
/// test in this binary so a stray failure can be located in logs.
const SURFACE_ID_A: u64 = 0x_544A_0001;
const SURFACE_ID_B: u64 = 0x_544A_0002;

/// Build a fresh bridge against the fixture's adapter. Each test gets
/// its own bridge so a poisoned guard from a prior test can't leak
/// into the next.
fn bridge_for(fixture: &HostFixture) -> CpuReadbackBridgeImpl {
    CpuReadbackBridgeImpl::new(Arc::clone(&fixture.adapter))
}

/// While the first acquire holds a write guard on `surface`, a second
/// `try_acquire(write)` for the same surface returns `Ok(None)`. After
/// the first guard drops, a follow-up `try_acquire(write)` succeeds.
#[test]
fn try_acquire_returns_none_when_write_held() {
    let Some(fixture) = HostFixture::try_new() else {
        println!("try_acquire_returns_none_when_write_held: no GPU — skipping");
        return;
    };
    let surface = fixture.register_surface(SURFACE_ID_A, 16, 16);
    let bridge = bridge_for(&fixture);

    // Hold a write via the blocking acquire path.
    let first = bridge
        .acquire(surface.id, CpuReadbackAccessMode::Write)
        .expect("first acquire (Write) must succeed against an unheld surface");

    // A non-blocking write while the first holder is alive must report
    // contention — Ok(None), no error.
    let second = bridge
        .try_acquire(surface.id, CpuReadbackAccessMode::Write)
        .expect("try_acquire must not surface contention as an error");
    assert!(
        second.is_none(),
        "try_acquire must return Ok(None) while a write guard is held"
    );

    // A non-blocking read while the writer is alive is also contended.
    let read_attempt = bridge
        .try_acquire(surface.id, CpuReadbackAccessMode::Read)
        .expect("try_acquire(Read) must not surface contention as an error");
    assert!(
        read_attempt.is_none(),
        "try_acquire(Read) must return Ok(None) while a write guard is held"
    );

    // Drop the holder. The adapter's Drop runs the CPU→GPU flush + the
    // timeline release-value signal so the next acquire can proceed.
    drop(first);

    // After the holder is gone, try_acquire must succeed again.
    let third = bridge
        .try_acquire(surface.id, CpuReadbackAccessMode::Write)
        .expect("try_acquire must not error after contention clears");
    assert!(
        third.is_some(),
        "try_acquire must return Ok(Some(_)) once the prior holder drops"
    );
    drop(third);
}

/// Multiple concurrent readers do NOT contend with each other — only
/// a writer-vs-reader or writer-vs-writer race produces `Ok(None)`.
/// Locks the read-sharing semantics that customers rely on for
/// shared-input fan-out (one writer upstream, many readers downstream).
#[test]
fn try_acquire_read_not_contended_by_concurrent_readers() {
    let Some(fixture) = HostFixture::try_new() else {
        println!(
            "try_acquire_read_not_contended_by_concurrent_readers: no GPU — skipping"
        );
        return;
    };
    let surface = fixture.register_surface(SURFACE_ID_B, 16, 16);
    let bridge = bridge_for(&fixture);

    let r1 = bridge
        .try_acquire(surface.id, CpuReadbackAccessMode::Read)
        .expect("first try_acquire(Read) on unheld surface")
        .expect("must succeed when no writer holds the surface");
    let r2 = bridge
        .try_acquire(surface.id, CpuReadbackAccessMode::Read)
        .expect("second try_acquire(Read) while another reader holds")
        .expect("multiple concurrent readers must coexist");
    drop(r1);
    drop(r2);

    // After both readers release, a writer can take the surface.
    let w = bridge
        .try_acquire(surface.id, CpuReadbackAccessMode::Write)
        .expect("try_acquire(Write) after readers release")
        .expect("write must succeed once read holders are gone");
    drop(w);
}

/// `try_acquire` against a surface that was never registered must
/// surface as `Err`, NOT `Ok(None)`. Distinguishes "surface not present"
/// (a configuration bug) from "surface present but contended" (the
/// expected non-blocking outcome). `_` prefix on the `_adapter` binding
/// is intentional — it owns the device the bridge needs.
#[test]
fn try_acquire_unknown_surface_id_returns_err() {
    let Some(fixture) = HostFixture::try_new() else {
        println!("try_acquire_unknown_surface_id_returns_err: no GPU — skipping");
        return;
    };
    let _adapter: Arc<CpuReadbackSurfaceAdapter> = Arc::clone(&fixture.adapter);
    let bridge = bridge_for(&fixture);
    // Pick an id we haven't registered. SurfaceId is u64; this number
    // can't collide with any other test's registration in this binary.
    let unknown_id = 0x_544A_FFFE_u64;
    let result = bridge.try_acquire(unknown_id, CpuReadbackAccessMode::Read);
    let err = match result {
        Ok(Some(_)) => panic!("unknown surface_id must not succeed"),
        Ok(None) => panic!(
            "unknown surface_id must produce Err, not Ok(None) — \
             contention is reserved for present-but-busy surfaces"
        ),
        Err(msg) => msg,
    };
    assert!(
        err.contains("not registered"),
        "expected 'not registered' in error, got: {err}"
    );
}
