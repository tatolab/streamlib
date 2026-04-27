// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Crash semantics for the cpu-readback adapter.
//!
//! Unlike the Vulkan/OpenGL adapters, which exist on both sides of an
//! FD-passing IPC boundary, the cpu-readback adapter is **intra-process
//! by construction** — the customer-facing `&[u8]` / `&mut [u8]` views
//! can't be shipped across a process boundary, so the "subprocess
//! holds a guard mid-acquire" failure mode doesn't apply here. The
//! relevant analog is a host thread that panics mid-write: the
//! [`WriteGuard`]'s `Drop` must still run via unwinding so the
//! per-surface state releases and the next `acquire_*` succeeds.
//!
//! This file covers both:
//!   * `panic_mid_write_releases_lock_for_next_acquire` — the
//!     host-side analog of the Vulkan crash test.
//!   * `unrelated_subprocess_crash_does_not_perturb_host_adapter` —
//!     uses [`SubprocessCrashHarness`] from
//!     `streamlib-adapter-abi::testing` to confirm the contract the
//!     issue body calls out, with a subprocess that does no shared
//!     work (the cpu-readback adapter has no FD-passed counterpart).

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use streamlib_adapter_abi::testing::{CrashTiming, SubprocessCrashHarness};

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
        // Touch the bytes so bytes_mut isn't optimized out.
        guard.view_mut().bytes_mut()[0] = 0xAB;
        panic!("simulated customer panic mid-write");
    }));
    assert!(result.is_err(), "the closure must have panicked");

    // Post-panic: the next acquire must succeed (lock released).
    {
        let guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("post-panic acquire_write must succeed");
        assert_eq!(guard.view().bytes().len(), 32 * 32 * 4);
    }
    {
        let _g = fixture
            .ctx
            .acquire_read(&descriptor)
            .expect("post-panic acquire_read must succeed");
    }
}

#[test]
fn unrelated_subprocess_crash_does_not_perturb_host_adapter() {
    let fixture = match common::HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "unrelated_subprocess_crash: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let descriptor = fixture.register_surface(2, 16, 16);

    // Warm up so the timeline has advanced.
    {
        let _w = fixture.ctx.acquire_write(&descriptor).expect("warm-up");
    }

    // Spawn a subprocess that will be killed by the harness — the
    // helper binary prints a role line and waits to be killed. The
    // cpu-readback adapter shares no resources with the subprocess;
    // the only invariant we assert is that the harness fires and the
    // host adapter is unchanged.
    let bin_path = env!("CARGO_BIN_EXE_cpu_readback_adapter_subprocess_helper");
    let mut cmd = Command::new(bin_path);
    cmd.arg("noop")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let observed = Arc::new(AtomicBool::new(false));
    let observed_clone = Arc::clone(&observed);

    let outcome = SubprocessCrashHarness::new(cmd)
        .with_timing(CrashTiming::AfterDelay(Duration::from_millis(50)))
        .with_cleanup_timeout(Duration::from_secs(2))
        .run(move || {
            observed_clone.store(true, Ordering::Release);
            Ok(())
        })
        .expect("crash harness must not error");

    assert!(observed.load(Ordering::Acquire));
    assert!(
        outcome.cleanup_latency.as_secs() < 2,
        "harness cleanup latency too high: {:?}",
        outcome.cleanup_latency
    );

    // Post-crash: host adapter is still healthy.
    {
        let _w = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("post-crash acquire_write");
    }
    {
        let _r = fixture
            .ctx
            .acquire_read(&descriptor)
            .expect("post-crash acquire_read");
    }
}
