// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The interpreter-lifecycle contract: engine teardown strictly precedes
//! interpreter finalization.
//!
//! Every blocking step runs with the GIL released, and the engine is dropped —
//! not merely stopped — before [`PythonRuntimeHandle::run`] returns, so no
//! engine thread can still be alive when CPython finalizes.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::runtime::Runner;

/// The engine, held by a Python object.
///
/// Single-use by construction: [`run`](PythonRuntimeHandle::run) takes the
/// engine out and drops it before returning, because "all engine threads
/// joined before `run()` returns" is not something a still-shared handle can
/// promise.
// `subclass` so the Python-side `streamlib.Runtime` can extend this to register
// itself with the `atexit` teardown hook.
#[pyclass(name = "Runtime", module = "streamlib", unsendable, subclass)]
pub struct PythonRuntimeHandle {
    /// `None` once the engine has been run or torn down.
    engine: Option<Arc<Runner>>,
}

impl PythonRuntimeHandle {
    /// Drop the engine with the GIL released, joining its threads.
    ///
    /// Releasing the GIL is not an optimization: an engine thread that needs
    /// the GIL to finish (a Python processor, once those exist) would deadlock
    /// against a teardown that held it.
    fn drop_engine_without_holding_the_gil(python: Python<'_>, engine: Arc<Runner>) {
        python.detach(move || {
            let Some(owned_engine) = Arc::into_inner(engine) else {
                // Something outlived the handle's reference, so dropping here
                // joins nothing. Fail loud rather than let `run()` return on a
                // contract it did not keep.
                tracing::error!(
                    "engine teardown left a live reference behind — engine threads may outlive \
                     interpreter finalization"
                );
                return;
            };
            drop(owned_engine);
        });
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
            engine: Some(engine),
        })
    }

    /// Run the pipeline until Ctrl-C or SIGTERM, then tear the engine down.
    ///
    /// Owns SIGINT while it blocks and hands it back to CPython before
    /// returning, so a later Ctrl-C raises `KeyboardInterrupt` as usual.
    fn run(&mut self, python: Python<'_>) -> PyResult<()> {
        let engine = self.engine.take().ok_or_else(|| {
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
        Self::drop_engine_without_holding_the_gil(python, engine);

        run_outcome.map_err(|engine_failure| PyRuntimeError::new_err(engine_failure.to_string()))
    }

    /// Tear the engine down if it has not been run. Idempotent.
    ///
    /// The escape hatch for the exception path — `streamlib`'s `atexit` hook
    /// and `__exit__` both land here.
    fn shutdown(&mut self, python: Python<'_>) {
        if let Some(engine) = self.engine.take() {
            Self::drop_engine_without_holding_the_gil(python, engine);
        }
    }

    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self
    }

    /// Never suppresses the exception — returning false lets it propagate once
    /// the engine is down.
    #[pyo3(signature = (*_exception_details))]
    fn __exit__(&mut self, python: Python<'_>, _exception_details: &Bound<'_, PyAny>) -> bool {
        self.shutdown(python);
        false
    }
}

impl Drop for PythonRuntimeHandle {
    /// Covers the garbage-collected path, where neither `__exit__` nor the
    /// `atexit` hook ran.
    fn drop(&mut self) {
        let Some(engine) = self.engine.take() else {
            return;
        };
        Python::attach(|python| {
            Self::drop_engine_without_holding_the_gil(python, engine);
        });
    }
}
