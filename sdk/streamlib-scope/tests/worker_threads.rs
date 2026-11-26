// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The scope's worker threads, counted by name in this process. Its own test binary, so no other
//! test's scope is counted with them.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

mod common;

use common::Counted;
use streamlib_scope::{ScopeBeingDeclared, computed};

/// The worker threads alive, waiting up to a second for any that are joined but not yet gone
/// from `/proc`, which lists a thread for a moment after its join returns.
fn scope_workers_settled_at(expected: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let running = scope_workers_running();
        if running == expected || Instant::now() >= deadline {
            return running;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn scope_workers_running() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .filter(|name| name.trim_end() == "scope-builds")
        .count()
}

#[test]
fn a_scope_starts_a_worker_only_for_a_computed_value() {
    assert_eq!(scope_workers_settled_at(0), 0);

    // Inputs alone: no worker. Writes still reach the frame, and the values the frame replaces
    // are dropped by the next write rather than kept.
    let alive = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let level = declared.input(Counted::new(0, &alive));
    let (scope, mut view) = declared.start().unwrap();
    assert_eq!(scope_workers_running(), 0);
    for value in 1..=50 {
        scope.write(|writes| writes.write(level, Counted::new(value, &alive)));
        assert_eq!(view.run_frame(|| level.get().value), Some(value));
        assert!(alive.load(Ordering::SeqCst) <= 2);
    }
    drop((scope, view));
    assert_eq!(alive.load(Ordering::SeqCst), 0);

    // A computed value: one worker, gone once the scope closes.
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let _kernel = declared.declare(|| computed(move || Ok::<_, String>(*taps.get() * 2)));
    let (scope, view) = declared.start().unwrap();
    assert!(scope.wait_until_settled(Duration::from_secs(5)));
    assert_eq!(scope_workers_running(), 1);
    drop((scope, view));
    assert_eq!(scope_workers_settled_at(0), 0);
}
