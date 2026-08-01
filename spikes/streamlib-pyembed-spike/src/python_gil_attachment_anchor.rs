// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeps a processor thread's CPython thread state alive for the thread's whole
//! lifetime, so per-frame `Python::attach` re-acquires the GIL instead of
//! rebuilding a thread state.
//!
//! Without an anchor, `Python::attach` maps to `PyGILState_Ensure()` on a
//! foreign thread, which creates a thread state on entry and destroys it on
//! exit. CPython 3.12 virtual-allocates the per-thread-state frame datastack
//! chunk on the first Python frame push and frees it on thread-state delete, so
//! every frame costs one `mmap` + one `munmap`. Measured on this rig: 1041 mmap
//! and 1005 munmap syscalls per 1000 callback invocations, p50 6.3µs per call.
//! Anchoring drops that to p50 110ns / p99 151ns — the syscall pair disappears.
//!
//! The mechanism is one `PyGILState_Ensure()` that is never released, parked
//! immediately with `PyEval_SaveThread()` so the thread does not hold the GIL
//! between frames. Public `pyo3::ffi` only; no engine change.
//!
//! This is a spike measurement device. Whether the real SDK should anchor
//! processor threads is a design question for the pivot, not something these
//! numbers settle — anchoring trades a resident thread state per processor
//! thread (a few KiB of datastack) for the per-frame syscall pair.

use std::marker::PhantomData;

/// Holds one processor thread's CPython thread state open.
///
/// Create it on the processor thread, drop it on that same thread. It is
/// deliberately neither `Send` nor `Sync`: CPython thread states are strictly
/// thread-confined, and releasing one from a different thread than the one that
/// created it corrupts the interpreter.
pub struct PythonGilAttachmentAnchorForProcessorThread {
    gil_state: pyo3::ffi::PyGILState_STATE,
    parked_thread_state: *mut pyo3::ffi::PyThreadState,
    // Anchors the !Send + !Sync obligation in the type system rather than in a
    // comment nobody reads at the call site.
    _thread_confined: PhantomData<*const ()>,
}

impl PythonGilAttachmentAnchorForProcessorThread {
    /// Attach this thread to the interpreter and park the GIL.
    ///
    /// The caller must not hold the GIL when calling this, and
    /// `Python::initialize()` must already have run.
    pub fn attach_current_thread_and_park_gil() -> Self {
        // Both preconditions are fatal-or-UB if violated: `PyGILState_Ensure` on
        // an uninitialized interpreter is a `Py_FatalError`, and a recursive
        // ensure followed by the unconditional `PyEval_SaveThread` below would
        // drop the GIL out from under an outer holder.
        debug_assert_ne!(
            unsafe { pyo3::ffi::Py_IsInitialized() },
            0,
            "Python::initialize() must run before a processor thread anchors"
        );
        debug_assert_eq!(
            unsafe { pyo3::ffi::PyGILState_Check() },
            0,
            "the anchoring thread must not already hold the GIL"
        );
        // SAFETY: the interpreter is initialized before any processor thread
        // starts (the harness calls `Python::initialize()` before `App::new`),
        // and this thread holds no thread state yet — `PyGILState_Ensure`
        // creates one and takes the GIL.
        let gil_state = unsafe { pyo3::ffi::PyGILState_Ensure() };
        // SAFETY: we hold the GIL from the `PyGILState_Ensure` above.
        // `PyEval_SaveThread` releases the GIL and returns the still-live
        // thread state, which we keep so `PyGILState_Release` can be paired
        // correctly in `Drop`.
        let parked_thread_state = unsafe { pyo3::ffi::PyEval_SaveThread() };
        Self {
            gil_state,
            parked_thread_state,
            _thread_confined: PhantomData,
        }
    }
}

impl Drop for PythonGilAttachmentAnchorForProcessorThread {
    fn drop(&mut self) {
        // SAFETY: restores the exact thread state parked in the constructor on
        // the same thread (enforced by !Send), reclaiming the GIL, then
        // releases the matching `PyGILState_Ensure`. Pairing is one-to-one.
        unsafe {
            pyo3::ffi::PyEval_RestoreThread(self.parked_thread_state);
            pyo3::ffi::PyGILState_Release(self.gil_state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monotonic_clock::read_monotonic_clock_nanoseconds;
    use pyo3::prelude::*;
    use std::sync::OnceLock;

    fn initialize_interpreter_once() {
        static INTERPRETER_INITIALIZED: OnceLock<()> = OnceLock::new();
        INTERPRETER_INITIALIZED.get_or_init(Python::initialize);
    }

    /// The anchor must not leave the GIL held — an anchored processor thread
    /// that kept the GIL between frames would serialize every other processor
    /// in the graph and invalidate every number the spike produces.
    #[test]
    fn anchor_does_not_hold_the_gil_between_frames() {
        initialize_interpreter_once();
        std::thread::spawn(|| {
            let _anchor = PythonGilAttachmentAnchorForProcessorThread::attach_current_thread_and_park_gil();
            // SAFETY: read-only query of the current thread's GIL ownership.
            let holds_gil_while_parked = unsafe { pyo3::ffi::PyGILState_Check() };
            assert_eq!(
                holds_gil_while_parked, 0,
                "the anchor must park the GIL, not hold it"
            );
            Python::attach(|_python| {
                // SAFETY: same read-only query, inside an attach scope.
                let holds_gil_inside_attach = unsafe { pyo3::ffi::PyGILState_Check() };
                assert_eq!(
                    holds_gil_inside_attach, 1,
                    "attach must reclaim the GIL for the callback body"
                );
            });
        })
        .join()
        .expect("anchored thread does not panic");
    }

    /// An anchored thread must still be able to call Python correctly — the
    /// anchor is a performance device and must not change semantics.
    #[test]
    fn anchored_thread_calls_python_correctly() {
        initialize_interpreter_once();
        let observed_result = std::thread::spawn(|| {
            let _anchor = PythonGilAttachmentAnchorForProcessorThread::attach_current_thread_and_park_gil();
            Python::attach(|python| {
                python
                    .eval(pyo3::ffi::c_str!("6 * 7"), None, None)
                    .expect("expression evaluates")
                    .extract::<i64>()
                    .expect("returns an int")
            })
        })
        .join()
        .expect("anchored thread does not panic");
        assert_eq!(observed_result, 42);
    }

    /// The anchor's reason for existing, asserted rather than assumed: it must
    /// make repeated attach+call materially cheaper than the unanchored path.
    ///
    /// The threshold is deliberately loose (2x) so the test asserts the
    /// mechanism works rather than pinning a machine-specific ratio — the
    /// measured ratio on the development rig was ~53x at p50. If this fails,
    /// the anchor is not doing what the whole design assumes.
    #[test]
    fn anchoring_is_materially_cheaper_than_rebuilding_the_thread_state() {
        initialize_interpreter_once();
        const CALIBRATION_ITERATIONS: usize = 20_000;

        fn median_attach_and_call_nanoseconds(anchored: bool) -> i64 {
            std::thread::spawn(move || {
                let _anchor = anchored.then(
                    PythonGilAttachmentAnchorForProcessorThread::attach_current_thread_and_park_gil,
                );
                let callable = Python::attach(|python| {
                    python
                        .eval(pyo3::ffi::c_str!("lambda value: value"), None, None)
                        .expect("lambda compiles")
                        .unbind()
                });
                let mut samples = Vec::with_capacity(CALIBRATION_ITERATIONS);
                for _ in 0..CALIBRATION_ITERATIONS {
                    let started = read_monotonic_clock_nanoseconds();
                    Python::attach(|python| {
                        let _ = callable.call1(python, (1i64,)).expect("call succeeds");
                    });
                    samples.push(read_monotonic_clock_nanoseconds() - started);
                }
                samples.sort_unstable();
                samples[samples.len() / 2]
            })
            .join()
            .expect("calibration thread does not panic")
        }

        let unanchored_median = median_attach_and_call_nanoseconds(false);
        let anchored_median = median_attach_and_call_nanoseconds(true);
        assert!(
            anchored_median * 2 < unanchored_median,
            "anchoring must materially beat thread-state rebuild, \
             got anchored p50 {anchored_median}ns vs unanchored p50 {unanchored_median}ns"
        );
    }
}
