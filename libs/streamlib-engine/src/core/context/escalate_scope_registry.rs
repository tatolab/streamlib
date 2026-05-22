// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-side registry that mints opaque scope tokens for the
//! cdylib vtable-dispatched
//! [`GpuContextLimitedAccess::escalate`] path.
//!
//! Two dispatch shapes coexist for [`GpuContextFullAccess`]:
//! engine-internal callers reach it directly via
//! [`GpuContextLimitedAccess::escalate`]'s in-process path
//! ([`GpuContextFullAccess::new`] wrapping an `Arc<GpuContext>`
//! clone); cdylib-resident callers cross the FFI through the
//! [`GpuContextLimitedAccessVTable`]'s `escalate_begin` callback,
//! which reaches this registry to bind a fresh `Arc<GpuContext>`
//! clone to a new `ScopeToken`. Every FullAccess vtable callback
//! validates the supplied scope token against the registry before
//! dispatch; the matching `escalate_end` callback removes the scope.
//!
//! Tokens are monotonically incrementing u64 serials (no ABA risk —
//! 2^64 unique tokens). The registry is a single global static; per-
//! GpuContext serialization is provided by each [`GpuContext`]'s own
//! [`EscalateGate`](crate::core::context::escalate_gate::EscalateGate)
//! which `begin_escalate_scope` enters and `end_escalate_scope`
//! releases. The registry holds only `Arc<GpuContext>` — no mutex
//! guard, no Send-cross-thread footgun — so a scope minted on one
//! thread can be released on another (panic recovery, async runtimes).
//!
//! [`GpuContextLimitedAccess::escalate`]: super::gpu_context::GpuContextLimitedAccess::escalate
//! [`GpuContextFullAccess`]: super::gpu_context::GpuContextFullAccess
//! [`GpuContextFullAccess::new`]: super::gpu_context::GpuContextFullAccess::new
//! [`GpuContextLimitedAccessVTable`]: streamlib_plugin_abi::GpuContextLimitedAccessVTable

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::gpu_context::GpuContext;

/// Opaque scope token identifying an active escalate scope.
///
/// Issued by [`begin_escalate_scope`], invalidated by
/// [`end_escalate_scope`]. Crosses the FFI as `*const c_void`; the
/// engine stores it as `u64` for HashMap keying.
pub(crate) type ScopeToken = u64;

struct EscalateScopeRegistry {
    scopes: Mutex<HashMap<ScopeToken, Arc<GpuContext>>>,
    next_serial: AtomicU64,
}

static REGISTRY: OnceLock<EscalateScopeRegistry> = OnceLock::new();

fn registry() -> &'static EscalateScopeRegistry {
    REGISTRY.get_or_init(|| EscalateScopeRegistry {
        scopes: Mutex::new(HashMap::new()),
        // Start at 1 so a 0 token is reserved for "invalid / never issued".
        next_serial: AtomicU64::new(1),
    })
}

/// Begin a fresh escalate scope. Enters the supplied context's
/// [`EscalateGate`](super::escalate_gate::EscalateGate) (blocking
/// until any prior scope ends), mints a unique token, and stores the
/// bound `Arc<GpuContext>` in the registry.
///
/// The returned token is opaque to the caller; the cdylib side holds
/// it for the duration of its escalate scope and passes it back to
/// [`end_escalate_scope`] when the scope completes.
pub(crate) fn begin_escalate_scope(arc_ctx: Arc<GpuContext>) -> ScopeToken {
    arc_ctx.escalate_gate().enter();
    let token = registry().next_serial.fetch_add(1, Ordering::Relaxed);
    let mut scopes = registry()
        .scopes
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    scopes.insert(token, arc_ctx);
    token
}

/// End an escalate scope. Removes the bound `Arc<GpuContext>` from
/// the registry and releases that context's escalate gate.
///
/// Returns `true` if the token was present (normal completion);
/// `false` if it was already removed (e.g. double `escalate_end` or a
/// never-issued token). Idempotent on the registry — duplicate calls
/// don't release another scope's gate.
///
/// **Caller contract.** The caller MUST ensure no FullAccess vtable
/// call against this scope token is still in-flight on another
/// thread when `end_escalate_scope` runs. Releasing the gate early
/// while a FullAccess method is mid-execution would let a fresh
/// `begin_escalate_scope` overlap with the tail of the prior scope's
/// GPU work. The cdylib's `escalate_via_vtable` wrapper enforces
/// this naturally — the closure runs synchronously and returns
/// before `escalate_end` fires. Cdylib code that spawns a thread
/// inside an escalate closure and lets it outlive the scope is a
/// caller bug.
pub(crate) fn end_escalate_scope(token: ScopeToken) -> bool {
    let removed = {
        let mut scopes = registry()
            .scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scopes.remove(&token)
    };
    match removed {
        Some(arc_ctx) => {
            arc_ctx.escalate_gate().exit();
            true
        }
        None => false,
    }
}

/// Look up the `Arc<GpuContext>` bound to an active scope, then invoke
/// the closure against it. Returns `None` if the token is invalidated
/// or never-issued (vtable callbacks return
/// [`crate::core::error::Error::InvalidEscalateScope`] in that case).
///
/// The registry lock is released before the closure runs — the Arc
/// clone keeps the `GpuContext` alive for the closure's duration even
/// if a concurrent `end_escalate_scope` removes the scope mid-call.
pub(crate) fn with_scope<F, R>(token: ScopeToken, f: F) -> Option<R>
where
    F: FnOnce(&Arc<GpuContext>) -> R,
{
    let arc_clone = {
        let scopes = registry()
            .scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scopes.get(&token).cloned()
    };
    arc_clone.as_ref().map(f)
}

#[cfg(test)]
mod tests {
    //! Tests that construct a real `GpuContext` carry `#[serial]` to
    //! prevent the NVIDIA Linux dual-`VkDevice` SIGSEGV
    //! (`docs/learnings/nvidia-dual-vulkan-device-crash.md`) when run
    //! against other VkDevice-creating tests in the workspace lib
    //! suite. The single test that doesn't create a context
    //! (`with_scope_returns_none_for_never_issued_token`) doesn't
    //! need it.

    use super::*;
    use crate::core::context::GpuContext;
    use serial_test::serial;

    fn new_arc_ctx() -> Option<Arc<GpuContext>> {
        // GpuContext::init_for_platform skips cleanly when no GPU
        // device is available; the gpu_context-level escalate tests
        // (test_escalate_serializes_concurrent_callers,
        // test_escalate_propagates_closure_error) exercise the registry
        // end-to-end under real Vulkan, while these unit tests pin the
        // registry's data-structure invariants when a device is
        // present.
        GpuContext::init_for_platform().ok().map(Arc::new)
    }

    #[test]
    #[serial]
    fn begin_returns_distinct_tokens_per_call() {
        let Some(arc) = new_arc_ctx() else {
            tracing::warn!(
                "escalate_scope_registry test skipped: init_for_platform failed (no GPU)"
            );
            return;
        };
        let t1 = begin_escalate_scope(Arc::clone(&arc));
        // First scope holds the lock; end it before begin-2 to avoid a
        // deadlock — begin acquires the processor-setup lock and
        // calling begin again on the same Arc would block until t1
        // ends.
        assert!(end_escalate_scope(t1));
        let t2 = begin_escalate_scope(Arc::clone(&arc));
        assert!(end_escalate_scope(t2));
        assert_ne!(t1, t2, "tokens must be unique per begin call");
    }

    #[test]
    #[serial]
    fn with_scope_returns_none_after_end() {
        let Some(arc) = new_arc_ctx() else {
            tracing::warn!(
                "escalate_scope_registry test skipped: init_for_platform failed (no GPU)"
            );
            return;
        };
        let token = begin_escalate_scope(Arc::clone(&arc));
        assert!(
            with_scope(token, |_| ()).is_some(),
            "with_scope must succeed during an active scope"
        );
        assert!(end_escalate_scope(token));
        assert!(
            with_scope(token, |_| ()).is_none(),
            "with_scope must fail after end_escalate_scope"
        );
    }

    #[test]
    fn with_scope_returns_none_for_never_issued_token() {
        // Token u64::MAX is never issued in any practical test (the
        // serial would need to reach 2^64). Validates the missing-key
        // path independent of any active scope state. No GpuContext
        // construction, so no #[serial] needed.
        assert!(with_scope(u64::MAX, |_| ()).is_none());
    }

    #[test]
    #[serial]
    fn end_returns_false_for_stale_token() {
        let Some(arc) = new_arc_ctx() else {
            tracing::warn!(
                "escalate_scope_registry test skipped: init_for_platform failed (no GPU)"
            );
            return;
        };
        let token = begin_escalate_scope(Arc::clone(&arc));
        assert!(end_escalate_scope(token));
        // Double-end is idempotent — returns false rather than
        // panicking or releasing another scope's lock.
        assert!(!end_escalate_scope(token));
    }
}
