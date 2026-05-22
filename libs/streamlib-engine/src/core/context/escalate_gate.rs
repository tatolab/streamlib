// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Exclusivity gate that serializes
//! [`GpuContextLimitedAccess::escalate`] scopes against each other
//! across both the engine-internal in-process dispatch path and the
//! cdylib vtable-dispatched path.
//!
//! Unlike a [`std::sync::Mutex`] guard, this gate doesn't materialize
//! an OS-level lock guard that has to be held across a stack frame —
//! enter and exit are independent calls that can run on different
//! threads. The cdylib's vtable-dispatched path needs that
//! flexibility because the FFI `escalate_begin` callback returns
//! (unwinding its stack) before the cdylib runs its closure; the
//! matching `escalate_end` callback can reach `exit` from a different
//! thread (panic recovery, async runtimes). A regular Mutex guard
//! would have to live in shared state crossing threads, which
//! `std::sync::MutexGuard` is `!Send` for — this gate sidesteps that
//! with a `Mutex<bool>` flag + `Condvar`.
//!
//! [`GpuContextLimitedAccess::escalate`]: super::gpu_context::GpuContextLimitedAccess::escalate

use std::sync::{Condvar, Mutex};

/// Exclusivity gate for escalate scopes on a single
/// [`GpuContext`](super::gpu_context::GpuContext).
///
/// `enter` blocks until any active scope releases (via `exit`), then
/// marks the gate as in-scope. `exit` clears the in-scope flag and
/// wakes one waiter. Same-thread re-entry is NOT supported — calling
/// `enter` twice from the same thread without an intervening `exit`
/// deadlocks.
pub(crate) struct EscalateGate {
    in_scope: Mutex<bool>,
    cv: Condvar,
}

impl EscalateGate {
    pub(crate) fn new() -> Self {
        Self {
            in_scope: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    /// Block until the gate is free, then claim it. Pairs with
    /// [`Self::exit`]. The companion [`Self::enter_scoped`] returns
    /// an RAII guard that releases on drop (engine-internal use).
    pub(crate) fn enter(&self) {
        let mut guard = self
            .in_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while *guard {
            guard = self
                .cv
                .wait(guard)
                .unwrap_or_else(|e| e.into_inner());
        }
        *guard = true;
        // Mutex<bool> drops here — the gate is held by the `true`
        // flag, not by any guard. Other `enter` callers see `true` on
        // their `while` check and wait on the Condvar.
    }

    /// Release the gate. Wakes one waiter (if any).
    ///
    /// Calling `exit` without a matching `enter` clears a flag that
    /// was already false — harmless but indicates a bug at the call
    /// site.
    pub(crate) fn exit(&self) {
        let mut guard = self
            .in_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = false;
        self.cv.notify_one();
    }

    /// Enter the gate and return an RAII guard that releases on drop.
    /// Suited for the engine-internal in-process
    /// [`GpuContextLimitedAccess::escalate`] path, where the scope
    /// lives within a single Rust stack frame. The cdylib
    /// vtable-dispatched path uses bare [`Self::enter`] /
    /// [`Self::exit`] via the
    /// [`escalate_scope_registry`](super::escalate_scope_registry)
    /// (the FFI boundary precludes RAII across it).
    ///
    /// [`GpuContextLimitedAccess::escalate`]: super::gpu_context::GpuContextLimitedAccess::escalate
    pub(crate) fn enter_scoped(&self) -> EscalateGateGuard<'_> {
        self.enter();
        EscalateGateGuard { gate: self }
    }
}

impl Default for EscalateGate {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard returned by [`EscalateGate::enter_scoped`] that calls
/// [`EscalateGate::exit`] on drop.
pub(crate) struct EscalateGateGuard<'a> {
    gate: &'a EscalateGate,
}

impl Drop for EscalateGateGuard<'_> {
    fn drop(&mut self) {
        self.gate.exit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn enter_exit_pair_unblocks_subsequent_enter() {
        let gate = EscalateGate::new();
        gate.enter();
        gate.exit();
        // Second enter must not block — gate is free again.
        gate.enter();
        gate.exit();
    }

    #[test]
    fn enter_scoped_releases_on_drop() {
        let gate = EscalateGate::new();
        {
            let _guard = gate.enter_scoped();
            // gate is held inside this scope
        }
        // After guard drop, second enter must not block.
        gate.enter();
        gate.exit();
    }

    #[test]
    fn enter_serializes_concurrent_callers() {
        let gate = Arc::new(EscalateGate::new());
        let overlap = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));

        const THREADS: usize = 8;
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let overlap = Arc::clone(&overlap);
                let active = Arc::clone(&active);
                std::thread::spawn(move || {
                    gate.enter();
                    if active.fetch_add(1, Ordering::SeqCst) != 0 {
                        overlap.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    active.fetch_sub(1, Ordering::SeqCst);
                    gate.exit();
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
        assert_eq!(
            overlap.load(Ordering::SeqCst),
            0,
            "EscalateGate must serialize concurrent enters"
        );
    }

    #[test]
    fn cross_thread_enter_exit_pair_works() {
        // Mirrors the cdylib FFI pattern: one thread calls enter
        // (escalate_begin); a different thread may call exit
        // (escalate_end), e.g. when the cdylib delegates the close to
        // a worker thread for panic recovery.
        let gate = Arc::new(EscalateGate::new());
        gate.enter();
        let g2 = Arc::clone(&gate);
        std::thread::spawn(move || g2.exit())
            .join()
            .expect("worker panicked");
        // Gate must be free again.
        gate.enter();
        gate.exit();
    }
}
