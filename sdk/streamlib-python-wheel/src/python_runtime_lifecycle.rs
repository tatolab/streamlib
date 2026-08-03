// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The interpreter-lifecycle contract: engine teardown strictly precedes
//! interpreter finalization.
//!
//! Every blocking step runs with the GIL released, and the engine is dropped —
//! not merely stopped — before [`PythonRuntimeHandle::run`] returns, so no
//! engine thread can still be alive when CPython finalizes.

use std::sync::{Arc, Mutex, MutexGuard};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::runtime::{
    Runner, request_runtime_shutdown, take_runtime_shutdown_request_latch,
};

/// A reference to the engine outlived the handle, so its threads were not
/// joined and the teardown contract was not kept.
struct EngineTeardownIncomplete;

impl std::fmt::Display for EngineTeardownIncomplete {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "engine teardown left a live reference behind — engine threads may outlive \
             interpreter finalization",
        )
    }
}

impl From<EngineTeardownIncomplete> for PyErr {
    fn from(teardown_failure: EngineTeardownIncomplete) -> Self {
        PyRuntimeError::new_err(teardown_failure.to_string())
    }
}

/// Where the handle is in its one-way lifecycle.
///
/// One locked value rather than an engine slot plus a "running" flag, because
/// `shutdown()` must decide *and act* without the run loop's exit racing it: the
/// shutdown-request latch is process-global and first-observer-wins, so a
/// request issued after the run loop stopped observing is inherited by the next
/// run loop in the interpreter, which then returns having run nothing.
enum PythonRuntimeLifecycleState {
    EngineConstructedNotYetRun(Arc<Runner>),
    RunLoopBlockedUntilShutdownRequested,
    EngineTornDownAndThreadsJoined,
}

/// The engine, held by a Python object.
///
/// Single-use by construction: [`run`](PythonRuntimeHandle::run) takes the
/// engine out and drops it before returning.
// `subclass` so the Python-side `streamlib.Runtime` can extend this to register
// itself with the `atexit` teardown hook.
#[pyclass(name = "Runtime", module = "streamlib", subclass)]
pub struct PythonRuntimeHandle {
    lifecycle: Mutex<PythonRuntimeLifecycleState>,
}

impl PythonRuntimeHandle {
    fn lifecycle(&self) -> MutexGuard<'_, PythonRuntimeLifecycleState> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop the engine with the GIL released, joining its threads.
    ///
    /// Releasing the GIL is not an optimization: an engine thread that needs
    /// the GIL to finish (a Python processor, once those exist) would deadlock
    /// against a teardown that held it.
    fn drop_engine_without_holding_the_gil(
        python: Python<'_>,
        engine: Arc<Runner>,
    ) -> Result<(), EngineTeardownIncomplete> {
        python.detach(move || {
            // `start()` parks an `Arc<Runner>` inside the `RuntimeContext` it
            // stores on the runner, and only `stop()` clears it. Without this
            // the cycle survives every path where the run loop did not stop the
            // engine itself — a failed `start()`, or a handle torn down before
            // it ever ran — and `into_inner` below would join nothing.
            if let Err(stop_failure) = engine.stop() {
                tracing::warn!(%stop_failure, "engine stop reported a failure during teardown");
            }

            match Arc::into_inner(engine) {
                Some(owned_engine) => {
                    drop(owned_engine);
                    Ok(())
                }
                // Something still holds a reference, so the threads are not
                // joined.
                None => Err(EngineTeardownIncomplete),
            }
        })
    }
}

#[pymethods]
impl PythonRuntimeHandle {
    /// Boot the engine.
    #[new]
    fn new(python: Python<'_>) -> PyResult<Self> {
        let engine = python
            .detach(Runner::new)
            .map_err(|engine_failure| PyRuntimeError::new_err(engine_failure.to_string()))?;
        Ok(Self {
            lifecycle: Mutex::new(PythonRuntimeLifecycleState::EngineConstructedNotYetRun(
                engine,
            )),
        })
    }

    /// Run the pipeline until Ctrl-C, SIGTERM, or [`shutdown`], then tear the
    /// engine down.
    ///
    /// Owns SIGINT while it blocks and hands it back to CPython before
    /// returning, so a later Ctrl-C raises `KeyboardInterrupt` as usual.
    ///
    /// Call it from the main thread. On a worker thread the interpreter can
    /// begin finalizing while this is still inside teardown, and the thread is
    /// killed the moment it reattaches.
    ///
    /// Linux only, matching the platform floor. On macOS the engine's run loop
    /// is an `NSApplication` loop that terminates the process instead of
    /// returning, so none of the above holds there — not the return, not the
    /// teardown, not the handback.
    ///
    /// [`shutdown`]: PythonRuntimeHandle::shutdown
    fn run(&self, python: Python<'_>) -> PyResult<()> {
        let engine = {
            let mut lifecycle = self.lifecycle();
            match std::mem::replace(
                &mut *lifecycle,
                PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested,
            ) {
                PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) => engine,
                already_run => {
                    *lifecycle = already_run;
                    return Err(PyRuntimeError::new_err(
                        "this Runtime has already been run; construct a new one to run again",
                    ));
                }
            }
        };

        // Owns the shutdown signals across startup as well as the wait: with the
        // GIL released here, a SIGINT that reached CPython's handler instead
        // could never become a `KeyboardInterrupt`, and this call would block
        // forever.
        let run_outcome = python.detach(|| engine.start_and_wait_for_shutdown());

        {
            let mut lifecycle = self.lifecycle();
            // Taken under the same lock `shutdown()` holds while it requests, so
            // a request issued in the window between the run loop's last
            // observation and this transition is consumed here rather than left
            // for the next run loop in this interpreter.
            take_runtime_shutdown_request_latch();
            *lifecycle = PythonRuntimeLifecycleState::EngineTornDownAndThreadsJoined;
        }

        // Unconditional: a failed start must still not leave engine threads
        // alive to race interpreter finalization.
        let teardown_outcome = Self::drop_engine_without_holding_the_gil(python, engine);

        run_outcome
            .map_err(|engine_failure| PyRuntimeError::new_err(engine_failure.to_string()))?;
        Ok(teardown_outcome?)
    }

    /// Ask the pipeline to stop, and tear the engine down if it never ran.
    ///
    /// Safe to call from any thread. While `run()` is blocking this is what
    /// ends it — the request goes through the same funnel Ctrl-C does, and
    /// `run()` performs the teardown. Before `run()`, it tears the engine down
    /// here. After teardown it does nothing. Idempotent in every case.
    ///
    /// It returns as soon as the request is issued; when `run()` is blocking on
    /// another thread, teardown completes on that thread rather than this one.
    fn shutdown(&self, python: Python<'_>) -> PyResult<()> {
        let mut lifecycle = self.lifecycle();
        match &*lifecycle {
            PythonRuntimeLifecycleState::EngineConstructedNotYetRun(_) => {
                let PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) =
                    std::mem::replace(
                        &mut *lifecycle,
                        PythonRuntimeLifecycleState::EngineTornDownAndThreadsJoined,
                    )
                else {
                    unreachable!("matched EngineConstructedNotYetRun under the same lock")
                };
                drop(lifecycle);
                Ok(Self::drop_engine_without_holding_the_gil(python, engine)?)
            }
            PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested => {
                // Issued while still holding the lock: `run()` takes it to move
                // to the torn-down state and clears the latch there, so this
                // request cannot outlive the run loop it is meant for.
                request_runtime_shutdown("streamlib.Runtime.shutdown()")
                    .map_err(|request_failure| PyRuntimeError::new_err(request_failure.to_string()))
            }
            PythonRuntimeLifecycleState::EngineTornDownAndThreadsJoined => Ok(()),
        }
    }

    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self
    }

    /// Never suppresses the exception — returning false lets it propagate once
    /// the engine is down.
    #[pyo3(signature = (*_exception_details))]
    fn __exit__(
        &self,
        python: Python<'_>,
        _exception_details: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        self.shutdown(python)?;
        Ok(false)
    }
}

impl Drop for PythonRuntimeHandle {
    /// Covers the garbage-collected path, where neither `__exit__` nor the
    /// `atexit` hook ran.
    ///
    /// A `Drop` cannot report failure, so this is the one caller that may only
    /// log.
    fn drop(&mut self) {
        let engine = match std::mem::replace(
            &mut *self.lifecycle(),
            PythonRuntimeLifecycleState::EngineTornDownAndThreadsJoined,
        ) {
            PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) => engine,
            _ => return,
        };
        Python::attach(|python| {
            if let Err(teardown_failure) = Self::drop_engine_without_holding_the_gil(python, engine)
            {
                tracing::error!(%teardown_failure);
            }
        });
    }
}
