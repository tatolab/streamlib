// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `subprocess_crash_mid_write` — placeholder for a Skia-specific
//! crash test.
//!
//! `streamlib_adapter_abi::testing::SubprocessCrashHarness` covers
//! host-side cleanup when a polyglot subprocess holds a surface
//! guard and is `SIGKILL`ed mid-acquire. The Skia adapter composes on
//! `streamlib-adapter-vulkan` and does not allocate any per-acquire
//! GPU resources of its own — the wrapped `skia::Surface` /
//! `skia::Image` only borrow the inner Vulkan adapter's
//! resources. `streamlib-adapter-vulkan/tests/subprocess_crash_mid_write.rs`
//! already exercises the cleanup path for that inner adapter, and a
//! Skia subprocess crash exercises the same path.
//!
//! When the polyglot Skia wrapper lands (filed as a follow-up issue
//! against this milestone), this test should grow real Skia-side
//! coverage: a Python subprocess that opens a `skia.Surface`, draws,
//! and crashes mid-flush. For now, this file is a documented skip.

#![cfg(target_os = "linux")]

#[test]
fn subprocess_crash_test_deferred_to_polyglot_followup() {
    println!(
        "subprocess_crash_mid_write: deferred — Skia adapter does not allocate \
         per-acquire GPU resources beyond what the inner Vulkan adapter holds. \
         streamlib-adapter-vulkan/tests/subprocess_crash_mid_write.rs covers the \
         cleanup path Skia depends on; a Skia-specific subprocess test will land \
         alongside the polyglot Skia wrapper follow-up."
    );
}
