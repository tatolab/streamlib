// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The interpreter-lifecycle contract: engine teardown strictly precedes
//! interpreter finalization.
//!
//! Every blocking step runs with the GIL released, and the engine is dropped —
//! not merely stopped — before [`PythonRuntimeHandle::run`] returns, so no
//! engine thread can still be alive when CPython finalizes.

use std::sync::{Arc, Mutex};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::runtime::{Runner, request_runtime_shutdown};

/// A reference to the engine outlived the handle, so its threads were not
/// joined and the teardown contract was not kept.
struct EngineTeardownIncomplete;

impl From<EngineTeardownIncomplete> for PyErr {
    fn from(_: EngineTeardownIncomplete) -> Self {
        PyRuntimeError::new_err(
            "engine teardown left a live reference behind — engine threads may outlive \
             interpreter finalization",
        )
    }
}

/// The engine, held by a Python object.
///
/// Single-use by construction: [`run`](PythonRuntimeHandle::run) takes the
/// engine out and drops it before returning, because "all engine threads
/// joined before `run()` returns" is not something a still-shared handle can
/// promise.
// `subclass` so the Python-side `streamlib.Runtime` can extend this to register
// itself with the `atexit` teardown hook.
#[pyclass(name = "Runtime", module = "streamlib", subclass)]
pub struct PythonRuntimeHandle {
    /// `None` once the engine has been run or torn down.
    ///
    /// Behind a lock rather than `&mut self` so every method takes `&self`: a
    /// `&mut self` `run()` keeps the pyclass mutably borrowed for the whole
    /// blocking call, and `shutdown()` from a worker thread then fails on the
    /// borrow instead of stopping the pipeline.
    engine: Mutex<Option<Arc<Runner>>>,
}

impl PythonRuntimeHandle {
    /// Take the engine out, leaving the handle empty. Whoever gets `Some` owns
    /// the teardown.
    fn take_engine(&self) -> Option<Arc<Runner>> {
        self.engine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
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
                // Reached only if something still holds a reference, which means
                // the threads are not joined. The caller raises rather than
                // returning on a contract it did not keep.
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
            engine: Mutex::new(Some(engine)),
        })
    }

    /// Run the pipeline until Ctrl-C or SIGTERM, then tear the engine down.
    ///
    /// Owns SIGINT while it blocks and hands it back to CPython before
    /// returning, so a later Ctrl-C raises `KeyboardInterrupt` as usual.
    fn run(&self, python: Python<'_>) -> PyResult<()> {
        let engine = self.take_engine().ok_or_else(|| {
            PyRuntimeError::new_err(
                "this Runtime has already been run; construct a new one to run again",
            )
        })?;

        // Owns the shutdown signals across startup as well as the wait: with the
        // GIL released here, a SIGINT that reached CPython's handler instead
        // could never become a `KeyboardInterrupt`, and this call would block
        // forever.
        let run_outcome = python.detach(|| engine.start_and_wait_for_shutdown());

        // Unconditional: a failed start must still not leave engine threads
        // alive to race interpreter finalization.
        let teardown_outcome = Self::drop_engine_without_holding_the_gil(python, engine);

        // The run failure is the more informative one, so it wins if both fired.
        run_outcome
            .map_err(|engine_failure| PyRuntimeError::new_err(engine_failure.to_string()))?;
        Ok(teardown_outcome?)
    }

    /// Ask the pipeline to stop, and tear the engine down if it never ran.
    ///
    /// Safe to call from any thread and at any point: while `run()` is
    /// blocking, this is what ends it — the request goes through the same
    /// funnel Ctrl-C does, and `run()` performs the teardown. Before `run()`,
    /// it tears the engine down here. Idempotent either way.
    fn shutdown(&self, python: Python<'_>) -> PyResult<()> {
        match self.take_engine() {
            Some(engine) => Ok(Self::drop_engine_without_holding_the_gil(python, engine)?),
            // Either `run()` owns the engine and is blocking on it, or teardown
            // already happened — the request funnel is idempotent, so asking
            // again costs nothing and the second case is a no-op.
            None => python
                .detach(|| request_runtime_shutdown("streamlib.Runtime.shutdown()"))
                .map_err(|request_failure| PyRuntimeError::new_err(request_failure.to_string())),
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
    /// Only reachable while the handle still owns the engine; a `Drop` cannot
    /// report failure, so this is the one caller that may only log.
    fn drop(&mut self) {
        let Some(engine) = self.take_engine() else {
            return;
        };
        Python::attach(|python| {
            if Self::drop_engine_without_holding_the_gil(python, engine).is_err() {
                tracing::error!(
                    "engine teardown left a live reference behind — engine threads may outlive \
                     interpreter finalization"
                );
            }
        });
    }
}
