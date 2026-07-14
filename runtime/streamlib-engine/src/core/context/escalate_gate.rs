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
use std::thread::ThreadId;

/// Exclusivity gate for escalate scopes on a single
/// [`GpuContext`](super::gpu_context::GpuContext).
///
/// `enter` blocks until any active scope releases (via `exit`), then
/// marks the gate as in-scope. `exit` clears the in-scope flag and
/// wakes one waiter.
///
/// **Same-thread re-entry panics.** The historical contract is that
/// `escalate(|full| ...)` from inside a setup/teardown lifecycle
/// body (which already runs with the gate held by the spawn op) is
/// forbidden — it used to silently deadlock against the original
/// `processor_setup_lock: Arc<Mutex<()>>`. After PR #912 replaced
/// that mutex with this gate, the deadlock shape is the same; the
/// added thread tracking here turns the silent hang into a clear
/// panic at the call site. The panic is caught by the FFI boundary's
/// `run_host_extern_c` wrapper (in cdylib mode) and propagates as a
/// normal Rust panic for engine-internal callers, so the failure
/// surface is "your code panicked with an actionable message" rather
/// than "your test hangs forever."
///
/// The held-by-thread tracking is best-effort: it catches the
/// common case (paired same-thread enter/enter recursion) but does
/// NOT support recursive entry. Cross-thread enter/exit pairs are
/// still allowed — the cdylib FFI path needs that flexibility
/// because `escalate_begin` returns before the closure runs and the
/// matching `escalate_end` may fire from a different thread (panic
/// recovery, async runtimes).
pub(crate) struct EscalateGate {
    state: Mutex<GateState>,
    cv: Condvar,
}

struct GateState {
    in_scope: bool,
    /// Thread that called `enter` and hasn't yet called `exit`.
    /// `None` when the gate is free. Used solely to detect (and
    /// panic on) same-thread re-entry; cross-thread paired
    /// enter/exit clears this normally regardless of which thread
    /// holds it.
    holder: Option<ThreadId>,
}

impl EscalateGate {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                in_scope: false,
                holder: None,
            }),
            cv: Condvar::new(),
        }
    }

    /// Block until the gate is free, then claim it. Pairs with
    /// [`Self::exit`]. The companion [`Self::enter_scoped`] returns
    /// an RAII guard that releases on drop (engine-internal use).
    ///
    /// Panics if the gate is already held by the current thread —
    /// see the type-level doc for the rationale.
    pub(crate) fn enter(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let current = std::thread::current().id();
        while state.in_scope {
            if state.holder == Some(current) {
                panic!(
                    "EscalateGate::enter() called twice from the same thread \
                     ({current:?}) without an intervening exit — escalate-from-setup \
                     is forbidden by the sandbox contract. setup() / teardown() \
                     bodies already run with the gate held by the spawn op; \
                     call ctx.gpu_full_access() directly (cdylib-safe after #1072) \
                     instead of ctx.gpu_limited_access().escalate(|full| ...)."
                );
            }
            state = self.cv.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.in_scope = true;
        state.holder = Some(current);
        // Mutex<GateState> drops here — the gate is held by the
        // in_scope flag, not by any guard. Other `enter` callers see
        // in_scope=true on their `while` check and wait on the
        // Condvar.
    }

    /// Whether a scope is currently held (`enter` without a matching
    /// `exit`).
    ///
    /// Test-only invariant probe: used by the
    /// [`escalate_scope_registry`](super::escalate_scope_registry)
    /// regression test that locks "the device drain runs while the
    /// gate is still held." Not a general-purpose API — the held
    /// state can change between this read and any action taken on it,
    /// so it must not be used for control flow.
    #[cfg(test)]
    pub(crate) fn in_scope(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_scope
    }

    /// Release the gate. Wakes one waiter (if any).
    ///
    /// Calling `exit` without a matching `enter` clears a flag that
    /// was already false — harmless but indicates a bug at the call
    /// site.
    pub(crate) fn exit(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.in_scope = false;
        state.holder = None;
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

    #[test]
    fn same_thread_reentry_panics() {
        // The historical sandbox contract forbids
        // `escalate(|full| ...)` from inside a setup/teardown body
        // (the spawn op already holds the gate). The gate's enter
        // detects same-thread re-entry and panics with an actionable
        // message rather than silently deadlocking — see the type
        // doc for the rationale.
        let gate = EscalateGate::new();
        gate.enter();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gate.enter();
        }));
        // Release the outer enter so other tests sharing global
        // state aren't affected (this gate is local, but documenting
        // the discipline).
        gate.exit();
        let panic_payload = result.expect_err(
            "EscalateGate::enter must panic on same-thread re-entry, not deadlock or succeed",
        );
        let msg = panic_payload
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| panic_payload.downcast_ref::<&'static str>().copied())
            .unwrap_or("<non-string panic>");
        assert!(
            msg.contains("escalate-from-setup is forbidden"),
            "expected escalate-from-setup panic message, got: {msg}"
        );
    }
}
