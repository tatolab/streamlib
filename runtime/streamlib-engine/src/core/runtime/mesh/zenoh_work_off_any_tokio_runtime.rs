// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one thread every blocking Zenoh call in this module is handed to.
//!
//! **No Zenoh call may run on a current-thread tokio runtime** — Zenoh resolves
//! its builders by blocking on its own pool, which panics there. `Runner::new()`
//! and `stop()` are called from whatever thread an app happens to own, and
//! `streamlib nodes` from whatever thread the CLI happens to own, so none of
//! them assumes: each hands its Zenoh work to a thread of this module's own.

/// Run `zenoh_work` on a thread that is nobody's tokio runtime.
///
/// A scoped thread rather than a detached one: the caller has to have the
/// result before it goes on. A thread the OS will not give is reported, because
/// the mesh never fails a runtime's start; a panic inside comes back out
/// unchanged, because that is not the mesh's to swallow.
pub(super) fn off_any_current_thread_tokio_runtime<T: Send>(
    mesh_step: &str,
    zenoh_work: impl FnOnce() -> T + Send,
) -> std::io::Result<T> {
    std::thread::scope(|threads| {
        let thread = std::thread::Builder::new()
            .name(format!("streamlib-mesh-{mesh_step}"))
            .spawn_scoped(threads, zenoh_work)?;
        match thread.join() {
            Ok(done) => Ok(done),
            Err(panicked) => std::panic::resume_unwind(panicked),
        }
    })
}
