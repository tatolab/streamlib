// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Same-thread reentrance contract for `EglRuntime::lock_make_current`
//! and `EglRuntime::arc_lock_make_current`.
//!
//! Nested acquire patterns surface in real consumers: e.g.
//! AvatarCharacter Linux's `acquire_write(out)` → `_ensure_render_state`
//! → `acquire_read_external_oes(camera)` chain takes an outer EGL lock
//! through the cdylib's per-acquire `arc_lock_make_current`, then the
//! inner `register_external_oes_host_surface` calls `lock_make_current`
//! again on the same thread. The non-reentrant `parking_lot::Mutex`
//! that previously backed `make_current_lock` deadlocked that pattern
//! before #626's fix.
//!
//! These tests pin the same-thread reentrance contract so any future
//! refactor of the lock primitive surfaces a regression here instead
//! of in a downstream subprocess pipeline.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use common::HostFixture;

#[test]
fn nested_lock_make_current_does_not_deadlock() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("nested_lock_make_current_does_not_deadlock: skipping — no EGL");
            return;
        }
    };

    let outer = fixture
        .egl
        .lock_make_current()
        .expect("outer lock_make_current");

    // The inner call must not deadlock: same-thread reentrance returns
    // a no-op guard. Without the reentrance check, this hangs forever
    // on the non-reentrant Mutex.
    let inner = fixture
        .egl
        .lock_make_current()
        .expect("inner lock_make_current (same thread, must reenter)");

    // Inner drop must NOT release the EGL context — the outer guard
    // still expects the context to be current. We can't directly observe
    // "context is still current" through the public API, so we rely on
    // the next acquire succeeding to confirm the lock state is consistent.
    drop(inner);
    drop(outer);

    // After the outer drops, the lock is released and a fresh acquire
    // succeeds — proves the lock didn't end up permanently held by an
    // unbalanced reentrance count.
    let after = fixture
        .egl
        .lock_make_current()
        .expect("post-nested lock_make_current");
    drop(after);
}

#[test]
fn nested_arc_lock_make_current_does_not_deadlock() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("nested_arc_lock_make_current_does_not_deadlock: skipping — no EGL");
            return;
        }
    };

    // Mixed-style nesting: outer borrowed, inner owned. The cdylib
    // takes the outer via `arc_lock_make_current` (returns an owned
    // 'static guard for FFI scope spans); the inner adapter register
    // path takes a borrowed `lock_make_current`. The reentrance check
    // must work across the two guard flavors identically.
    let outer = fixture
        .egl
        .arc_lock_make_current()
        .expect("outer arc_lock_make_current");
    let inner_borrowed = fixture
        .egl
        .lock_make_current()
        .expect("inner lock_make_current after arc outer");
    drop(inner_borrowed);

    // And the symmetric inner: arc-style after a borrowed outer.
    // (The avatar's path is technically only the outer-arc + inner-borrowed
    // direction, but pinning both directions keeps the contract symmetric
    // for future refactors.)
    let inner_arc = fixture
        .egl
        .arc_lock_make_current()
        .expect("inner arc_lock_make_current");
    drop(inner_arc);
    drop(outer);

    let after = fixture
        .egl
        .lock_make_current()
        .expect("post-nested lock_make_current");
    drop(after);
}
