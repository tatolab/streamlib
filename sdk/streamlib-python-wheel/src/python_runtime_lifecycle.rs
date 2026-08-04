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
use pyo3::types::PyDict;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{
    Runner, request_runtime_shutdown, take_runtime_shutdown_request_latch,
};

use crate::python_added_processor::{
    PythonAddedProcessor, PythonProcessorInputPortReference, PythonProcessorOutputPortReference,
};
use crate::python_bag_conversion::python_object_to_json_value;
use crate::python_processor_registration::register_processor_class;

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

    /// Take a reference to the engine to build the graph, before the run loop
    /// owns it.
    ///
    /// Graph building happens between construction and `run()`; once the run
    /// loop has taken the engine there is no handle left to add to, which is
    /// what makes this a lifecycle error rather than a missing feature.
    ///
    /// Returns an owned `Arc` rather than lending the guard's contents, so the
    /// lock is released before the caller detaches. Holding it across a
    /// GIL-released engine call deadlocks: `detach` re-attaches before it
    /// returns, so this thread would wait for the GIL while holding the lock
    /// that `run()` and `shutdown()` take *with* the GIL held.
    ///
    /// What that trades away: a `shutdown()` landing while an `add` still holds
    /// its clone makes teardown's `Arc::into_inner` return `None` and report an
    /// incomplete teardown. Harmless here and only here — this state is
    /// pre-`start()`, so the engine owns no threads for the report to be about,
    /// and the adder's clone drops moments later. The alternative is making
    /// teardown wait out an in-flight `add`, which reintroduces the wait this
    /// exists to avoid.
    fn engine_being_built(&self, what: &str) -> PyResult<Arc<Runner>> {
        match &*self.lifecycle() {
            PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) => Ok(engine.clone()),
            PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested => {
                Err(PyRuntimeError::new_err(format!(
                    "cannot {what}: this Runtime is already running. Build the whole graph \
                     before calling run()."
                )))
            }
            PythonRuntimeLifecycleState::EngineTornDownAndThreadsJoined => {
                Err(PyRuntimeError::new_err(format!(
                    "cannot {what}: this Runtime has been shut down. Construct a new one."
                )))
            }
        }
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

    /// Add a processor class to the graph.
    ///
    /// Takes the class, not an instance. `config` becomes the keyword arguments
    /// the class is constructed with — which happens later, on the engine's
    /// compile thread as `run()` brings the graph up, so a failing `__init__`
    /// surfaces from `run()` rather than from here. Adding the same class twice
    /// gives two processors, each with its own instance and configuration.
    #[pyo3(signature = (processor_class, *, config = None, display_name = None))]
    fn add(
        &self,
        python: Python<'_>,
        processor_class: &Bound<'_, PyAny>,
        config: Option<&Bound<'_, PyDict>>,
        display_name: Option<String>,
    ) -> PyResult<PythonAddedProcessor> {
        if !crate::python_processor_declaration::is_declared_processor_class(processor_class) {
            return Err(PyRuntimeError::new_err(format!(
                "{} is not a processor: decorate the class with @streamlib.processor, and pass \
                 the class itself rather than an instance of it",
                processor_class
            )));
        }

        // Before registering: registration writes to the process-global
        // registry, and a runtime that can no longer be built should not leave
        // a processor type behind for a node that will never exist.
        let engine = self.engine_being_built("add a processor")?;

        let type_reference = register_processor_class(python, processor_class)?;
        let configuration = match config {
            Some(config) => python_object_to_json_value(config.as_any())?,
            None => serde_json::Value::Null,
        };

        // The same rule the graph applies when it names the node, so the handle
        // and `streamlib graph` agree without a round trip to ask.
        let display_name =
            display_name.unwrap_or_else(|| type_reference.r#type().as_str().to_string());
        let spec = ProcessorSpec::new(type_reference, configuration)
            .with_display_name(display_name.clone());

        let processor_id = python
            .detach(|| engine.add_processor(spec))
            .map_err(|add_failure| PyRuntimeError::new_err(add_failure.to_string()))?;
        Ok(PythonAddedProcessor::new(
            processor_id.as_str().to_string(),
            display_name,
        ))
    }

    /// Link one processor's output port to another's input port.
    fn connect(
        &self,
        python: Python<'_>,
        source: &PythonProcessorOutputPortReference,
        destination: &PythonProcessorInputPortReference,
    ) -> PyResult<()> {
        let from = OutputLinkPortRef::new(source.processor_id.clone(), source.port_name.clone());
        let to = InputLinkPortRef::new(
            destination.processor_id.clone(),
            destination.port_name.clone(),
        );
        let engine = self.engine_being_built("connect two processors")?;
        python
            .detach(|| engine.connect(from, to))
            .map(|_link_id| ())
            .map_err(|connect_failure| PyRuntimeError::new_err(connect_failure.to_string()))
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
