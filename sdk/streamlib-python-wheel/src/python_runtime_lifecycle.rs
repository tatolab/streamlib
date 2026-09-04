// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The interpreter-lifecycle contract: engine teardown strictly precedes
//! interpreter finalization.
//!
//! Every blocking step runs with the GIL released, and the engine is dropped —
//! not merely stopped — before [`PythonRuntimeHandle::run`] returns, so no
//! engine thread can still be alive when CPython finalizes.

use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
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

/// What kind of thing `Runtime.add` was handed, resolved once up front.
enum AddedProcessorClassKind {
    /// A wheel-exported marker for a statically-linked native processor.
    NativeBuiltin(streamlib::sdk::descriptors::ProcessorClassImportPath),
    /// A class carrying the `@streamlib.processor` declaration.
    DeclaredPythonClass,
}

/// Classify `processor_class`, owning the not-a-processor rejection.
fn classify_processor_class(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<AddedProcessorClassKind> {
    if let Some(native_class) =
        crate::python_native_builtin_blocks::native_builtin_class_import_path(
            python,
            processor_class,
        )?
    {
        return Ok(AddedProcessorClassKind::NativeBuiltin(native_class));
    }
    if let Some(harness_class) =
        crate::python_test_harness_endpoints::test_harness_class_import_path(
            python,
            processor_class,
        )
    {
        return Ok(AddedProcessorClassKind::NativeBuiltin(harness_class));
    }
    if crate::python_processor_declaration::is_declared_processor_class(processor_class) {
        return Ok(AddedProcessorClassKind::DeclaredPythonClass);
    }
    Err(PyRuntimeError::new_err(format!(
        "{} is not a processor: decorate the class with @streamlib.processor, and pass \
         the class itself rather than an instance of it",
        processor_class
    )))
}

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
    /// Weak, never strong: the run loop owns the only strong reference, and a
    /// second one here would make teardown's `Arc::into_inner` find the engine
    /// still borrowed and report that its threads were never joined.
    RunLoopBlockedUntilShutdownRequested(Weak<Runner>),
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
            PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested(_) => {
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

    /// Take a reference to the engine to read the graph's readiness from.
    ///
    /// Answers in both live states, not just the running one. A processor
    /// carries its state from the moment it is added and the compiler
    /// transitions that same state rather than replacing it, so a wait started
    /// against a graph that has not been run yet is already watching the states
    /// `run()` will move. That is what lets a caller start the run loop on one
    /// thread and wait on another without sequencing the two — there is no
    /// window in which the wait arrives too early.
    ///
    /// The caller must drop this before waiting on what it reads. The run loop
    /// has to hold the only strong reference for teardown to join the engine's
    /// threads, and one kept alive across the wait would make `Arc::into_inner`
    /// report that it could not.
    fn engine_to_read_graph_readiness_from(&self, what: &str) -> PyResult<Arc<Runner>> {
        match &*self.lifecycle() {
            PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) => Ok(engine.clone()),
            PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested(engine) => {
                engine.upgrade().ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "cannot {what}: this Runtime's engine is being torn down."
                    ))
                })
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
    /// this interpreter's GIL to finish — a control-plane handler, a log
    /// drain — would deadlock against a teardown that held it.
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
        // Before the engine, so a processor added to its graph always has an
        // interpreter to be an exec of. This reads the app's own
        // `sys.executable`, which is the promise: one venv, and a processor's
        // child is the same Python the app is.
        crate::python_helper_process_spawn_host::capture_helper_process_launch_environment(python)?;
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
        let class_kind = classify_processor_class(python, processor_class)?;

        // Before registering: registration writes to the process-global
        // registry, and a runtime that can no longer be built should not leave
        // a processor type behind for a node that will never exist.
        let engine = self.engine_being_built("add a processor")?;

        // Native built-ins were registered at module import; only a Python
        // class needs registering here.
        let processor_class_import_path = match class_kind {
            AddedProcessorClassKind::NativeBuiltin(native_class) => native_class,
            AddedProcessorClassKind::DeclaredPythonClass => {
                register_processor_class(python, processor_class)?
            }
        };
        // An omitted `config` is an empty object, never null: a processor's
        // config type is a struct whose fields carry serde defaults, and a
        // struct deserializes from `{}` but not from `null`. Sending null made
        // `rt.add(CameraSource)` — the spelling the plan blesses for a block
        // that needs no configuration — fail at graph compile time.
        let configuration = match config {
            Some(config) => python_object_to_json_value(config.as_any())?,
            None => serde_json::Value::Object(serde_json::Map::new()),
        };

        // An absent `display_name` stays absent — the graph is the only place
        // that defaults a name, and the only place that disambiguates one.
        let mut spec = ProcessorSpec::new(processor_class_import_path, configuration);
        spec.display_name = display_name;

        let (processor_id, assigned_display_name) = python
            .detach(|| engine.add_processor_reporting_assigned_display_name(spec))
            .map_err(|add_failure| PyRuntimeError::new_err(add_failure.to_string()))?;
        Ok(PythonAddedProcessor::new(
            processor_id.as_str().to_string(),
            assigned_display_name,
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

    /// Host the control plane in this process, so the node is discoverable.
    ///
    /// Opt-in: a runtime that never calls this runs headless and publishes no
    /// node-registry entry. Called before `run()`, like every other
    /// graph-building call — the control plane is a processor in the graph.
    #[pyo3(signature = (*, bind_host = "0.0.0.0".to_string(), bind_port = 9000, node_name = None))]
    fn host_control_plane(
        &self,
        python: Python<'_>,
        bind_host: String,
        bind_port: u16,
        node_name: Option<String>,
    ) -> PyResult<()> {
        let engine = self.engine_being_built("host the control plane")?;
        crate::python_control_plane_hosting::host_control_plane_on_engine(
            python, &engine, bind_host, bind_port, node_name,
        )
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
            // Replaced with the terminal state first because the running state
            // needs the engine to point at, which is what is being taken here.
            match std::mem::replace(
                &mut *lifecycle,
                PythonRuntimeLifecycleState::EngineTornDownAndThreadsJoined,
            ) {
                PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) => {
                    *lifecycle = PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested(
                        Arc::downgrade(&engine),
                    );
                    engine
                }
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
            PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested(_) => {
                // Issued while still holding the lock: `run()` takes it to move
                // to the torn-down state and clears the latch there, so this
                // request cannot outlive the run loop it is meant for.
                request_runtime_shutdown("streamlib.Runtime.shutdown()")
                    .map_err(|request_failure| PyRuntimeError::new_err(request_failure.to_string()))
            }
            PythonRuntimeLifecycleState::EngineTornDownAndThreadsJoined => Ok(()),
        }
    }

    /// Block until every processor in the graph is running, then return.
    ///
    /// Call it around `run()` — before it, or from another thread while it
    /// blocks; a graph that has not started yet is waited through rather than
    /// refused. A processor runs once its `setup` has returned, and for a
    /// Python processor `setup` is what waits for its helper process to
    /// register and wire its ports. Publishing into the graph before that
    /// point loses bags: a link drops what it carries while its consumer is
    /// not yet attached.
    ///
    /// Raises if a processor failed instead of starting, or if `timeout`
    /// elapses first; the message names the processor and the state it was
    /// left in, so forgetting `run()` altogether reads as every processor
    /// still `Pending`.
    #[pyo3(signature = (*, timeout = 30.0))]
    fn wait_until_every_processor_is_running(
        &self,
        python: Python<'_>,
        timeout: f64,
    ) -> PyResult<()> {
        // Checked rather than `from_secs_f64`, which panics on a negative, a
        // NaN, or a value too large for a `Duration` — all reachable from
        // Python, none of them a reason to abort the interpreter.
        let timeout = Duration::try_from_secs_f64(timeout).map_err(|_| {
            PyValueError::new_err(format!(
                "timeout must be a finite, non-negative number of seconds, not {timeout}"
            ))
        })?;
        let engine = self.engine_to_read_graph_readiness_from("wait for the graph to come up")?;
        // Two detached steps rather than one, so the engine reference is gone
        // before the long one begins: reading the states needs the engine,
        // waiting on them does not. Detached because both take the graph lock,
        // which an engine thread can hold while it needs this interpreter's GIL.
        let graph_readiness = python.detach(|| {
            let graph_readiness = engine.observable_graph_readiness();
            drop(engine);
            graph_readiness
        });
        python
            .detach(|| graph_readiness.wait_until_every_processor_is_running(timeout))
            .map_err(|wait_failure| PyRuntimeError::new_err(wait_failure.to_string()))
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
