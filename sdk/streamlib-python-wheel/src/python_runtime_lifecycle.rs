// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The interpreter-lifecycle contract: engine teardown strictly precedes
//! interpreter finalization.
//!
//! Every blocking step runs with the GIL released, and the engine is dropped —
//! not merely stopped — before [`PythonRuntimeHandle::run`] returns, so every
//! engine thread is joined, or abandoned and named, when CPython finalizes.
//! Every teardown runs under the engine's watchdog.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::type_object::PyTypeInfo;
use pyo3::types::{PyDict, PyMapping, PyString};
use streamlib::engine_internal::core::app_directory::record_the_app_entry_directory_the_language_host_captured;
use streamlib::sdk::graph::cast_exposed_name_to_url_safe;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::runtime::{
    ArmedEngineTeardownWatchdog, DescriptionOfTheAbandonedProcessorThreads,
    ProcessorDisplayNameAndId, Runner, StreamEnvironment, request_runtime_shutdown,
    take_runtime_shutdown_escalation,
};

use crate::python_bag_conversion::python_object_to_json_value;

/// The engine could not be dropped, so its teardown did not finish.
enum EngineTeardownIncomplete {
    /// Processor threads ignored shutdown past their budget. The engine is left
    /// alive beneath them until the process exits, never dropped: a thread
    /// returning late would otherwise run the engine's drop — tokio's shutdown,
    /// the stdio restore, device wait-idle — on its own thread during
    /// interpreter finalization.
    ProcessorThreadsAbandoned(Vec<ProcessorDisplayNameAndId>),
    /// Something else still held a reference, so the threads were not joined.
    EngineStillReferenced,
}

impl std::fmt::Display for EngineTeardownIncomplete {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessorThreadsAbandoned(abandoned) => std::fmt::Display::fmt(
                &DescriptionOfTheAbandonedProcessorThreads(abandoned),
                formatter,
            ),
            Self::EngineStillReferenced => formatter.write_str(
                "engine teardown left a live reference behind — engine threads may outlive \
                 interpreter finalization",
            ),
        }
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
/// shutdown escalation is process-global and taken only when a run ends, so a
/// request issued after `run()` had taken it would be inherited by the next run
/// loop in the interpreter, which then returns having run nothing.
enum PythonRuntimeLifecycleState {
    EngineConstructedNotYetRun(Arc<Runner>),
    /// Weak, never strong: the run loop owns the only strong reference, and a
    /// second one here would make teardown's `Arc::into_inner` find the engine
    /// still borrowed and report that its threads were never joined.
    RunLoopBlockedUntilShutdownRequested(Weak<Runner>),
    EngineTornDownWithThreadsJoinedOrAbandoned,
}

/// What this Runtime's one `load` came to.
///
/// `run()` reads it because a load the engine refused partway can leave the
/// nodes it added before the refusal, and a Runtime must never run half a
/// graph. The claimed load's own refusal stands; otherwise the first refusal
/// recorded does.
#[derive(Debug, Clone, PartialEq)]
enum RuntimeGraphLoadRecord {
    NoGraphLoaded,
    /// A `load` claimed this Runtime's one load and has not returned yet.
    GraphLoadUnderway {
        /// The first call refused while the claimed load was underway, which
        /// stands over the claimed load's success but never over its refusal.
        first_refusal_while_underway: Option<String>,
    },
    GraphLoaded {
        stream_name: Option<String>,
    },
    GraphLoadRefused {
        refusal: String,
    },
}

impl RuntimeGraphLoadRecord {
    /// Lock `graph_load_record`, recovering it from a poisoned lock.
    fn locked(graph_load_record: &Mutex<Self>) -> MutexGuard<'_, Self> {
        graph_load_record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Claim this Runtime's one load, or record and return why it is spent.
    fn claim_the_one_load(&mut self) -> Result<(), String> {
        const ONE_RUNTIME_RUNS_ONE_STREAM: &str = "a Runtime takes exactly one load — one runtime \
                                                   runs one stream: construct another Runtime to \
                                                   load another";
        let refusal = match self {
            Self::NoGraphLoaded => {
                *self = Self::GraphLoadUnderway {
                    first_refusal_while_underway: None,
                };
                return Ok(());
            }
            Self::GraphLoadUnderway { .. } => format!(
                "another `load` of this Runtime is still underway, and {ONE_RUNTIME_RUNS_ONE_STREAM}"
            ),
            Self::GraphLoaded {
                stream_name: Some(stream_name),
            } => format!(
                "this Runtime already loaded the stream `{stream_name}`, and \
                 {ONE_RUNTIME_RUNS_ONE_STREAM}"
            ),
            Self::GraphLoaded { stream_name: None } => {
                format!("this Runtime already loaded a graph, and {ONE_RUNTIME_RUNS_ONE_STREAM}")
            }
            Self::GraphLoadRefused { refusal } => format!(
                "this Runtime's earlier load was refused: {refusal}. Construct a new Runtime and \
                 load a corrected graph into it"
            ),
        };
        self.record_a_refusal_that_claimed_no_load(&refusal);
        Err(refusal)
    }

    /// Record a refused `load` that never claimed the load; the first refusal
    /// stands.
    fn record_a_refusal_that_claimed_no_load(&mut self, refusal: &str) {
        match self {
            Self::NoGraphLoaded | Self::GraphLoaded { .. } => {
                *self = Self::GraphLoadRefused {
                    refusal: refusal.to_owned(),
                }
            }
            Self::GraphLoadUnderway {
                first_refusal_while_underway: first_refusal @ None,
            } => *first_refusal = Some(refusal.to_owned()),
            Self::GraphLoadUnderway {
                first_refusal_while_underway: Some(_),
            }
            | Self::GraphLoadRefused { .. } => {}
        }
    }

    /// Record how the load a successful claim began ended: its own refusal;
    /// else the first call refused while it was underway; else the stream
    /// name it loaded.
    fn record_the_claimed_load_outcome(&mut self, outcome: Result<Option<String>, String>) {
        let first_refusal_while_underway = match self {
            Self::GraphLoadUnderway {
                first_refusal_while_underway,
            } => first_refusal_while_underway.take(),
            _ => None,
        };
        *self = match (outcome, first_refusal_while_underway) {
            (Err(refusal), _) | (Ok(_), Some(refusal)) => Self::GraphLoadRefused { refusal },
            (Ok(stream_name), None) => Self::GraphLoaded { stream_name },
        };
    }

    /// Run the load a successful claim began and record how it ended. A panic
    /// is recorded as a refusal naming it before it resumes, so the record is
    /// never left underway.
    fn run_the_claimed_load<LoadOutcome>(
        graph_load_record: &Mutex<Self>,
        claimed_load: impl FnOnce() -> LoadOutcome,
        how_the_load_ended: impl FnOnce(&LoadOutcome) -> Result<Option<String>, String>,
    ) -> LoadOutcome {
        let load_outcome_and_how_it_ended = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let load_outcome = claimed_load();
            let how_it_ended = how_the_load_ended(&load_outcome);
            (load_outcome, how_it_ended)
        }));
        match load_outcome_and_how_it_ended {
            Ok((load_outcome, how_it_ended)) => {
                Self::locked(graph_load_record).record_the_claimed_load_outcome(how_it_ended);
                load_outcome
            }
            Err(panic_payload) => {
                let panic_message = panic_payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic_payload.downcast_ref::<&str>().copied())
                    .unwrap_or("a panic carrying no message");
                Self::locked(graph_load_record).record_the_claimed_load_outcome(Err(format!(
                    "the load panicked: {panic_message}"
                )));
                std::panic::resume_unwind(panic_payload)
            }
        }
    }

    /// Why `run()` must refuse, when this record says it must.
    fn refusal_of_run(&self) -> Option<String> {
        match self {
            Self::GraphLoadRefused { refusal } => Some(format!(
                "cannot run: a `load` on this Runtime was refused — {refusal}. A refused load can \
                 leave part of its graph behind, so a Runtime whose load was refused does not \
                 run: construct a new Runtime and load a corrected graph"
            )),
            Self::GraphLoadUnderway { .. } => Some(
                "cannot run: a `load` on this Runtime is still underway on another thread. Call \
                 run() once it returns"
                    .to_owned(),
            ),
            Self::NoGraphLoaded | Self::GraphLoaded { .. } => None,
        }
    }
}

/// The engine, held by a Python object.
///
/// Single-use by construction: [`run`](PythonRuntimeHandle::run) takes the
/// engine out and drops it before returning.
// `subclass` so the Python-side `tatolab.runtime.Runtime` can extend this to register
// itself with the `atexit` teardown hook.
#[pyclass(name = "Runtime", module = "tatolab.runtime", subclass)]
pub struct PythonRuntimeHandle {
    lifecycle: Mutex<PythonRuntimeLifecycleState>,
    /// Locked after `lifecycle` wherever both are held, so `run()`'s check of
    /// the record and `load()`'s claim of it cannot interleave.
    graph_load_record: Mutex<RuntimeGraphLoadRecord>,
}

impl PythonRuntimeHandle {
    fn lifecycle(&self) -> MutexGuard<'_, PythonRuntimeLifecycleState> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn graph_load_record(&self) -> MutexGuard<'_, RuntimeGraphLoadRecord> {
        RuntimeGraphLoadRecord::locked(&self.graph_load_record)
    }

    /// Take a reference to the engine to build the graph, before the run loop
    /// owns it.
    ///
    /// Graph building happens between construction and `run()`; once the run
    /// loop has taken the engine there is no handle left to load into, which
    /// is what makes this a lifecycle error rather than a missing feature.
    ///
    /// Returns an owned `Arc` rather than lending the guard's contents, so the
    /// lock is released before the caller detaches. Holding it across a
    /// GIL-released engine call deadlocks: `detach` re-attaches before it
    /// returns, so this thread would wait for the GIL while holding the lock
    /// that `run()` and `shutdown()` take *with* the GIL held.
    ///
    /// What that trades away: a `shutdown()` landing while a `load` still holds
    /// its clone makes teardown's `Arc::into_inner` return `None` and report an
    /// incomplete teardown. Harmless here and only here — this state is
    /// pre-`start()`, so the engine owns no threads for the report to be about,
    /// and the loader's clone drops moments later. The alternative is making
    /// teardown wait out an in-flight `load`, which reintroduces the wait this
    /// exists to avoid.
    fn engine_being_built(&self, what: &str) -> PyResult<Arc<Runner>> {
        Self::engine_being_built_in(&self.lifecycle(), what)
    }

    fn engine_being_built_in(
        lifecycle: &PythonRuntimeLifecycleState,
        what: &str,
    ) -> PyResult<Arc<Runner>> {
        match lifecycle {
            PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) => Ok(engine.clone()),
            PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested(_) => {
                Err(PyRuntimeError::new_err(format!(
                    "cannot {what}: this Runtime is already running. Build the whole graph \
                     before calling run()."
                )))
            }
            PythonRuntimeLifecycleState::EngineTornDownWithThreadsJoinedOrAbandoned => {
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
            PythonRuntimeLifecycleState::EngineTornDownWithThreadsJoinedOrAbandoned => {
                Err(PyRuntimeError::new_err(format!(
                    "cannot {what}: this Runtime has been shut down. Construct a new one."
                )))
            }
        }
    }

    /// Tear the engine down and drop it with the GIL released, under the
    /// engine's watchdog.
    ///
    /// Releasing the GIL is not an optimization: an engine thread that needs
    /// this interpreter's GIL to finish — a control-plane handler, a log
    /// drain — would deadlock against a teardown that held it.
    fn drop_engine_without_holding_the_gil(
        python: Python<'_>,
        engine: Arc<Runner>,
        teardown_name: &str,
    ) -> Result<(), EngineTeardownIncomplete> {
        python.detach(move || {
            let watchdog = ArmedEngineTeardownWatchdog::arm(teardown_name);
            // `start()` parks an `Arc<Runner>` inside the `RuntimeContext` it
            // stores on the runner, and only `stop()` clears it. Without this
            // the cycle survives every path where the run loop did not stop the
            // engine itself — a failed `start()`, or a handle torn down before
            // it ever ran — and `into_inner` below would join nothing.
            if let Err(stop_failure) = engine.stop() {
                tracing::warn!(%stop_failure, "engine stop reported a failure during teardown");
            }
            Self::drop_the_stopped_engine_unless_threads_were_abandoned(engine, &watchdog)
        })
    }

    /// Drop a stopped engine, or leave it alive beneath the processor threads
    /// abandoned on it.
    fn drop_the_stopped_engine_unless_threads_were_abandoned(
        engine: Arc<Runner>,
        _watched_by: &ArmedEngineTeardownWatchdog,
    ) -> Result<(), EngineTeardownIncomplete> {
        let abandoned = engine.processor_threads_abandoned_and_still_running();
        if !abandoned.is_empty() {
            // The error naming them is written to the process's standard
            // streams on its way out, which the engine would otherwise go on
            // intercepting until nothing is left to forward it.
            engine.stop_intercepting_the_standard_streams();
            std::mem::forget(engine);
            return Err(EngineTeardownIncomplete::ProcessorThreadsAbandoned(
                abandoned,
            ));
        }

        streamlib::sdk::runtime::note_what_the_engine_teardown_is_waiting_on(
            "the engine's own drop",
        );
        match Arc::into_inner(engine) {
            Some(owned_engine) => {
                drop(owned_engine);
                Ok(())
            }
            None => Err(EngineTeardownIncomplete::EngineStillReferenced),
        }
    }

    /// Mark the handle torn down, under the lock `shutdown()` takes, so a
    /// `shutdown()` from here on does nothing rather than request a shutdown of
    /// an engine on its way out.
    fn transition_to_torn_down(&self) {
        *self.lifecycle() = PythonRuntimeLifecycleState::EngineTornDownWithThreadsJoinedOrAbandoned;
    }
}

/// Every processor class import path in the calling process's catalog.
///
/// What `/api/registry` renders, reachable in a process that serves no control
/// plane.
#[pyfunction]
pub(crate) fn processor_class_import_paths_in_this_processes_catalog() -> Vec<String> {
    streamlib::sdk::processors::PROCESSOR_REGISTRY
        .registered_processor_class_import_paths()
        .into_iter()
        .map(|import_path| import_path.as_str().to_string())
        .collect()
}

/// The directory the app's own modules import from, as a runtime is named
/// after it.
///
/// `sys.path[0]` rather than `sys.argv[0]`'s parent, because it is the one slot
/// both launch paths agree on: CPython puts the script's directory there for a
/// hand-run `python <script>.py`, and `streamlib run` / `dev` inserts the
/// directory the entry imports from there before executing it. `sys.argv`
/// cannot answer this — the launcher narrows it to the entry only for the span
/// of that execution and restores its own argv in a `finally`, and the
/// `Runtime` is constructed *after* that.
///
/// Empty is `python -c`'s value for the slot and means the working directory,
/// which names no app.
fn app_import_root_directory(import_root: &str) -> Option<PathBuf> {
    if import_root.is_empty() {
        return None;
    }
    Path::new(import_root).canonicalize().ok()
}

/// The entry directory `sys.path[0]` names, if the interpreter reports one.
fn app_entry_directory_of_this_interpreter(python: Python<'_>) -> PyResult<Option<PathBuf>> {
    Ok(python
        .import("sys")?
        .getattr("path")?
        .get_item(0)
        .ok()
        .and_then(|import_root| import_root.extract::<String>().ok())
        .and_then(|import_root| app_import_root_directory(&import_root)))
}

/// The lend directory: the directory holding the `tatolab/runtime/` package
/// this interpreter imported the engine from, which every processor
/// interpreter borrows `tatolab.runtime` out of.
fn processor_interpreter_lend_directory_of_this_interpreter(
    python: Python<'_>,
) -> PyResult<PathBuf> {
    let package_init_file = PathBuf::from(
        python
            .import("tatolab.runtime")?
            .getattr("__file__")?
            .extract::<String>()?,
    );
    package_init_file
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "`tatolab.runtime` was imported from `{}`, which is not inside a                  `tatolab/runtime/` package directory, so there is no lend directory to start                  a processor interpreter from",
                package_init_file.display()
            ))
        })
}

/// The stream environment `Runtime.load` was passed, each path made absolute
/// against this process's working directory, so a relative one names the same
/// directory from inside the project a processor interpreter runs in.
fn the_stream_environment_load_was_passed(
    project_directory: &Bound<'_, PyAny>,
    interpreter: &Bound<'_, PyAny>,
) -> PyResult<StreamEnvironment> {
    let absolute = |argument_name: &str, path: PathBuf| -> PyResult<PathBuf> {
        std::path::absolute(&path).map_err(|cannot_be_made_absolute| {
            PyValueError::new_err(format!(
                "Runtime.load's `{argument_name}` `{}` cannot be made absolute: \
                 {cannot_be_made_absolute}",
                path.display()
            ))
        })
    };
    Ok(StreamEnvironment {
        project_directory: absolute(
            "project_directory",
            the_path_load_was_passed("project_directory", project_directory)?,
        )?,
        interpreter: absolute(
            "interpreter",
            the_path_load_was_passed("interpreter", interpreter)?,
        )?,
    })
}

/// A path argument `Runtime.load` was passed, as a path; anything but a `str`
/// or an `os.PathLike[str]` is refused naming the argument.
fn the_path_load_was_passed(argument_name: &str, path: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
    let fspath = path
        .py()
        .import("os")?
        .call_method1("fspath", (path,))
        .ok()
        .and_then(|fspath| fspath.cast_into::<PyString>().ok());
    let Some(fspath) = fspath else {
        return Err(PyTypeError::new_err(format!(
            "Runtime.load's `{argument_name}` takes a str or an os.PathLike[str], and was passed              a `{}`",
            path.get_type().name()?
        )));
    };
    Ok(PathBuf::from(fspath.to_str()?))
}

/// The mapping `Runtime.load` was passed, as a `dict` — itself when it is one,
/// else `dict(graph)`; anything that is not a `collections.abc.Mapping` is
/// refused naming what it is.
fn the_graph_mapping_load_was_passed<'py>(
    graph: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyDict>> {
    if let Ok(graph_dict) = graph.cast::<PyDict>() {
        return Ok(graph_dict.clone());
    }
    if graph.is_instance_of::<PyMapping>() {
        return Ok(PyDict::type_object(graph.py())
            .call1((graph,))?
            .cast_into::<PyDict>()?);
    }
    Err(PyTypeError::new_err(format!(
        "Runtime.load takes a graph mapping, and was passed a `{}`: pass the mapping \
         compile_stream_to_graph returns, or a graph `streamlib graph` rendered",
        graph.get_type().name()?
    )))
}

/// The stream name `Runtime.load` was passed; anything but a `str` that
/// encodes as UTF-8 is refused naming the fix.
fn the_stream_name_load_was_passed(name: Option<&Bound<'_, PyAny>>) -> PyResult<Option<String>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let Ok(name) = name.cast::<PyString>() else {
        return Err(PyTypeError::new_err(format!(
            "Runtime.load's `name` names the stream, and was passed a `{}`: pass a str, or leave \
             `name` out to keep the graph's own `stream`",
            name.get_type().name()?
        )));
    };
    match name.to_str() {
        Ok(stream_name) => Ok(Some(stream_name.to_owned())),
        Err(cannot_be_encoded) => {
            let python = name.py();
            let refusal = PyValueError::new_err(format!(
                "Runtime.load's `name` names the stream, and was passed a str that cannot be \
                 encoded as UTF-8 ({}): pass a str without lone surrogates",
                cannot_be_encoded.value(python)
            ));
            refusal.set_cause(python, Some(cannot_be_encoded));
            Err(refusal)
        }
    }
}

/// The converter's refusal of a graph, re-raised naming the graph and the fix,
/// with the converter's error as its `__cause__`. An error that refuses no
/// value — a `MemoryError`, a `KeyboardInterrupt` — passes through unchanged.
fn graph_is_not_json_data_refusal(python: Python<'_>, converter_refusal: PyErr) -> PyErr {
    let graph_refusal_type = if converter_refusal.is_instance_of::<PyTypeError>(python) {
        PyTypeError::type_object(python)
    } else if converter_refusal.is_instance_of::<PyValueError>(python) {
        PyValueError::type_object(python)
    } else {
        return converter_refusal;
    };
    let graph_refusal = PyErr::from_type(
        graph_refusal_type,
        format!(
            "the graph is not JSON data: {}. A graph carries what JSON carries — \
             compile_stream_to_graph always emits plain dicts, lists, str, int, float, bool and \
             None, so build the graph with it.",
            converter_refusal
                .value(python)
                .to_string()
                .trim_end_matches('.')
        ),
    );
    graph_refusal.set_cause(python, Some(converter_refusal));
    graph_refusal
}

/// Read, name and load the graph a claimed `load` was passed, reporting the
/// cast stream name it loaded under.
fn load_the_claimed_graph(
    python: Python<'_>,
    engine: &Arc<Runner>,
    graph_mapping: &Bound<'_, PyDict>,
    stream_name_override: Option<String>,
    stream_environment: StreamEnvironment,
) -> PyResult<Option<String>> {
    let graph_document = python_object_to_json_value(graph_mapping.as_any())
        .map_err(|converter_refusal| graph_is_not_json_data_refusal(python, converter_refusal))?;
    let mut graph_snapshot = GraphSnapshot::from_graph_document(graph_document)
        .map_err(|does_not_parse| PyRuntimeError::new_err(does_not_parse.to_string()))?;

    graph_snapshot.stream = stream_name_override
        .or(graph_snapshot.stream.take())
        .map(|stream_name| {
            cast_exposed_name_to_url_safe(&stream_name)
                .map(|cast| cast.into_owned())
                .map_err(|casts_to_nothing| {
                    PyValueError::new_err(format!(
                        "cannot load the graph as the stream `{stream_name}`: {casts_to_nothing}"
                    ))
                })
        })
        .transpose()?;

    if graph_snapshot.nodes.is_empty() {
        let what_holds_no_node = match &graph_snapshot.stream {
            Some(stream_name) => format!("the stream `{stream_name}`"),
            None => "the graph".to_owned(),
        };
        return Err(PyRuntimeError::new_err(format!(
            "{what_holds_no_node} holds no node — a stream whose function adds nothing compiles \
             to an empty graph, and there is nothing to run. Add a node with `stream_builder.add(...)`"
        )));
    }

    // Detached because the load takes the graph lock, which an engine thread
    // can hold while it needs this interpreter's GIL.
    python
        .detach(|| engine.load_graph_snapshot(&graph_snapshot, Some(stream_environment)))
        .map_err(|load_failure| PyRuntimeError::new_err(load_failure.to_string()))?;
    Ok(graph_snapshot.stream)
}

#[pymethods]
impl PythonRuntimeHandle {
    /// Boot the engine.
    #[new]
    #[pyo3(signature = (*, runtime_name = None))]
    fn new(python: Python<'_>, runtime_name: Option<String>) -> PyResult<Self> {
        // So an unnamed runtime in a hand-run `python <script>.py` is named
        // after the script's directory rather than after whatever shell it was
        // launched from. Only the interpreter knows it; the engine cannot read
        // `sys.path` for itself.
        if let Some(entry_directory) = app_entry_directory_of_this_interpreter(python)? {
            record_the_app_entry_directory_the_language_host_captured(entry_directory);
        }
        let processor_interpreter_lend_directory =
            processor_interpreter_lend_directory_of_this_interpreter(python)?;
        let engine = python
            .detach(|| Runner::new_with_runtime_name(runtime_name))
            .map_err(|engine_failure| PyRuntimeError::new_err(engine_failure.to_string()))?;
        engine.set_processor_interpreter_lend_directory(processor_interpreter_lend_directory);
        Ok(Self {
            lifecycle: Mutex::new(PythonRuntimeLifecycleState::EngineConstructedNotYetRun(
                engine,
            )),
            graph_load_record: Mutex::new(RuntimeGraphLoadRecord::NoGraphLoaded),
        })
    }

    /// Load a graph — the mapping `compile_stream_to_graph` returns, or one
    /// `streamlib graph` rendered — into this Runtime before `run()`.
    ///
    /// A Runtime takes exactly one `load`. Every refused call is recorded —
    /// save one refused because this Runtime is already running or shut down,
    /// which `run()` refuses anyway — and so is a panic inside the load; `run()`
    /// then refuses, naming the load's own refusal or panic, else the first
    /// refusal recorded.
    ///
    /// `project_directory` and `interpreter` are the stream's environment:
    /// every processor interpreter starts as `interpreter`, in
    /// `project_directory`, and every node type that is not a built-in is
    /// described there.
    #[pyo3(signature = (graph, *, project_directory, interpreter, name = None))]
    fn load(
        &self,
        python: Python<'_>,
        graph: &Bound<'_, PyAny>,
        project_directory: &Bound<'_, PyAny>,
        interpreter: &Bound<'_, PyAny>,
        name: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let (graph_mapping, stream_name_override, stream_environment) =
            the_graph_mapping_load_was_passed(graph)
                .and_then(|graph_mapping| {
                    Ok((
                        graph_mapping,
                        the_stream_name_load_was_passed(name)?,
                        the_stream_environment_load_was_passed(project_directory, interpreter)?,
                    ))
                })
                .inspect_err(|refusal| {
                    self.graph_load_record()
                        .record_a_refusal_that_claimed_no_load(&refusal.value(python).to_string());
                })?;
        let engine = {
            let lifecycle = self.lifecycle();
            let engine = Self::engine_being_built_in(&lifecycle, "load a graph")?;
            self.graph_load_record()
                .claim_the_one_load()
                .map_err(PyRuntimeError::new_err)?;
            engine
        };

        RuntimeGraphLoadRecord::run_the_claimed_load(
            &self.graph_load_record,
            move || {
                load_the_claimed_graph(
                    python,
                    &engine,
                    &graph_mapping,
                    stream_name_override,
                    stream_environment,
                )
            },
            |load_outcome| match load_outcome {
                Ok(stream_name) => Ok(stream_name.clone()),
                Err(refusal) => Err(refusal.value(python).to_string()),
            },
        )
        .map(|_stream_name| ())
    }

    /// Host the control plane in this process, so the node is discoverable.
    ///
    /// Opt-in: a runtime that never calls this runs headless and publishes no
    /// node-registry entry. Called before `run()`, like every other
    /// graph-building call — the control plane is a processor in the graph.
    fn host_control_plane(&self, python: Python<'_>) -> PyResult<()> {
        let engine = self.engine_being_built("host the control plane")?;
        crate::python_control_plane_hosting::host_control_plane_on_engine(python, &engine)
    }

    /// Run the pipeline until Ctrl-C, SIGTERM, SIGHUP or [`shutdown`], then tear
    /// the engine down.
    ///
    /// Owns SIGINT, SIGTERM and SIGHUP from startup until the engine is dropped,
    /// and hands them back to CPython before returning, so a later Ctrl-C raises
    /// `KeyboardInterrupt` as usual. The first interrupt stops the graph
    /// gracefully; the second forces it — every helper's process group is
    /// terminated without its teardown, and a native processor thread still in
    /// its callback is abandoned; the third kills every helper's process group
    /// and exits the process with status 130 at once.
    ///
    /// Raises `RuntimeError` naming each processor, by display name and id,
    /// whose thread ignored shutdown past its budget and was abandoned; the
    /// engine then stays alive beneath it until the process exits. A forced
    /// shutdown that abandoned nothing returns normally. A teardown still hung
    /// after about fifteen seconds ends the process with status 124.
    ///
    /// Call it from the main thread. On a worker thread the interpreter can
    /// begin finalizing while this is still inside teardown, and the thread is
    /// killed the moment it reattaches.
    ///
    /// On macOS the main thread drives the window event pump while this
    /// blocks, so a window opens only under `run()`. There the SIGINT and
    /// SIGTERM handlers are installed once for the process's life and never
    /// handed back, and SIGHUP is not owned.
    ///
    /// [`shutdown`]: PythonRuntimeHandle::shutdown
    fn run(&self, python: Python<'_>) -> PyResult<()> {
        let engine = {
            let mut lifecycle = self.lifecycle();
            let refusal_of_run = self.graph_load_record().refusal_of_run();
            if let Some(refusal_of_run) = refusal_of_run {
                return Err(PyRuntimeError::new_err(refusal_of_run));
            }
            // Replaced with the terminal state first because the running state
            // needs the engine to point at, which is what is being taken here.
            match std::mem::replace(
                &mut *lifecycle,
                PythonRuntimeLifecycleState::EngineTornDownWithThreadsJoinedOrAbandoned,
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

        let (run_outcome, teardown_outcome, this_run_owned_the_shutdown_signals) =
            python.detach(|| {
                // Held from before startup until the engine is dropped. Across
                // startup, because with the GIL released here a SIGINT that reached
                // CPython's handler could never become a `KeyboardInterrupt`; through
                // the drop, so a second and third interrupt escalate wherever the
                // teardown is.
                let (shutdown_signals, run_outcome) = match Runner::take_shutdown_signal_ownership()
                {
                    Ok(shutdown_signals) => {
                        let run_outcome =
                            engine.start_and_block_until_shutdown_is_requested(&shutdown_signals);
                        (Some(shutdown_signals), run_outcome)
                    }
                    Err(ownership_failure) => (None, Err(ownership_failure)),
                };

                // Unconditional: a failed start must still not leave engine threads
                // alive to race interpreter finalization.
                let watchdog =
                    ArmedEngineTeardownWatchdog::arm("the engine teardown Runtime.run() began");
                let stop_outcome = engine.stop();
                // Before the drop: `Arc::into_inner` needs the only strong
                // reference, and a readiness wait upgrading the running state's weak
                // one would otherwise find the engine still borrowed.
                self.transition_to_torn_down();
                let teardown_outcome =
                    Self::drop_the_stopped_engine_unless_threads_were_abandoned(engine, &watchdog);
                let this_run_owned_the_shutdown_signals = shutdown_signals.is_some();
                drop(shutdown_signals);
                drop(watchdog);
                (
                    run_outcome.and(stop_outcome),
                    teardown_outcome,
                    this_run_owned_the_shutdown_signals,
                )
            });

        // After the signals were handed back, so nothing can escalate this run
        // any further, and after the transition above, so no `shutdown()` can
        // request one either: what this run observed must not reach the next
        // run loop in this interpreter. A run that was refused the signals leaves
        // the escalation alone: it belongs to the run loop that owns them.
        if this_run_owned_the_shutdown_signals {
            take_runtime_shutdown_escalation();
        }

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
                        PythonRuntimeLifecycleState::EngineTornDownWithThreadsJoinedOrAbandoned,
                    )
                else {
                    unreachable!("matched EngineConstructedNotYetRun under the same lock")
                };
                drop(lifecycle);
                Ok(Self::drop_engine_without_holding_the_gil(
                    python,
                    engine,
                    "the engine teardown Runtime.shutdown() began",
                )?)
            }
            PythonRuntimeLifecycleState::RunLoopBlockedUntilShutdownRequested(_) => {
                // Issued while still holding the lock: `run()` takes it to move
                // to the torn-down state before its teardown and clears the
                // escalation only after it, so this request cannot outlive the
                // run loop it is meant for.
                request_runtime_shutdown("tatolab.runtime.Runtime.shutdown()")
                    .map_err(|request_failure| PyRuntimeError::new_err(request_failure.to_string()))
            }
            PythonRuntimeLifecycleState::EngineTornDownWithThreadsJoinedOrAbandoned => Ok(()),
        }
    }

    /// Block until every node in the graph is running, then return.
    ///
    /// Call it around `run()` — before it, or from another thread while it
    /// blocks; a graph that has not started yet is waited through rather than
    /// refused. A node runs once its `setup` has returned, and for a
    /// Python node `setup` is what waits for its helper process to
    /// register and wire its ports. Publishing into the graph before that
    /// point loses bags: a link drops what it carries while its consumer is
    /// not yet attached.
    ///
    /// Raises if a node failed instead of starting, or if `timeout`
    /// elapses first; the message names the node and the state it was
    /// left in, so forgetting `run()` altogether reads as every node
    /// still `Pending`.
    #[pyo3(name = "wait_until_every_node_is_running", signature = (*, timeout = 30.0))]
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
            PythonRuntimeLifecycleState::EngineTornDownWithThreadsJoinedOrAbandoned,
        ) {
            PythonRuntimeLifecycleState::EngineConstructedNotYetRun(engine) => engine,
            _ => return,
        };
        Python::attach(|python| {
            if let Err(teardown_failure) = Self::drop_engine_without_holding_the_gil(
                python,
                engine,
                "the engine teardown a dropped Runtime began",
            ) {
                tracing::error!(%teardown_failure);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sys.path[0]`, not `sys.argv[0]`: the launcher restores its own argv
    /// before the `Runtime` is built, so an argv-derived root is the wheel's
    /// own package directory and names no app.
    #[test]
    fn an_empty_import_root_is_no_root_rather_than_the_filesystem_root() {
        assert!(app_import_root_directory("").is_none());
        assert!(app_import_root_directory("/definitely/not/a/real/path").is_none());
        let real = std::env::temp_dir();
        assert_eq!(
            app_import_root_directory(&real.to_string_lossy()),
            real.canonicalize().ok()
        );
    }

    #[test]
    fn a_runtime_takes_its_one_load_and_runs_after_it_succeeds() {
        let mut record = RuntimeGraphLoadRecord::NoGraphLoaded;
        assert_eq!(record.refusal_of_run(), None);
        record.claim_the_one_load().unwrap();
        assert_eq!(
            record,
            RuntimeGraphLoadRecord::GraphLoadUnderway {
                first_refusal_while_underway: None
            }
        );
        record.record_the_claimed_load_outcome(Ok(Some("camera-rig".to_owned())));
        assert_eq!(record.refusal_of_run(), None);
    }

    #[test]
    fn a_second_load_after_a_success_is_refused_naming_the_stream_and_the_fix() {
        let mut record = RuntimeGraphLoadRecord::GraphLoaded {
            stream_name: Some("camera-rig".to_owned()),
        };
        let refusal = record.claim_the_one_load().unwrap_err();
        assert!(refusal.contains("`camera-rig`"), "{refusal}");
        assert!(
            refusal.contains("construct another Runtime to load another"),
            "{refusal}"
        );
        assert_eq!(
            record,
            RuntimeGraphLoadRecord::GraphLoadRefused {
                refusal: refusal.clone()
            }
        );
        assert!(record.refusal_of_run().unwrap().contains(&refusal));
    }

    #[test]
    fn a_second_load_of_a_graph_with_no_stream_name_is_refused_all_the_same() {
        let mut record = RuntimeGraphLoadRecord::GraphLoaded { stream_name: None };
        let refusal = record.claim_the_one_load().unwrap_err();
        assert!(refusal.contains("already loaded a graph"), "{refusal}");
    }

    #[test]
    fn a_load_after_a_refused_one_names_that_refusal_and_the_first_refusal_stands() {
        let mut record = RuntimeGraphLoadRecord::NoGraphLoaded;
        record.claim_the_one_load().unwrap();
        record.record_the_claimed_load_outcome(Err("the graph holds no node".to_owned()));

        let refusal = record.claim_the_one_load().unwrap_err();
        assert!(
            refusal.contains("earlier load was refused: the graph holds no node"),
            "{refusal}"
        );
        assert!(refusal.contains("Construct a new Runtime"), "{refusal}");
        assert_eq!(
            record,
            RuntimeGraphLoadRecord::GraphLoadRefused {
                refusal: "the graph holds no node".to_owned()
            }
        );
    }

    #[test]
    fn run_refuses_after_a_refused_load_naming_the_refusal_and_the_fix() {
        let mut record = RuntimeGraphLoadRecord::NoGraphLoaded;
        record.record_a_refusal_that_claimed_no_load("Runtime.load takes a graph mapping");
        let refusal_of_run = record.refusal_of_run().unwrap();
        assert!(
            refusal_of_run.contains("Runtime.load takes a graph mapping"),
            "{refusal_of_run}"
        );
        assert!(
            refusal_of_run.contains("construct a new Runtime and load a corrected graph"),
            "{refusal_of_run}"
        );
    }

    #[test]
    fn a_claimed_load_that_returns_a_refusal_records_it() {
        let graph_load_record = Mutex::new(RuntimeGraphLoadRecord::NoGraphLoaded);
        RuntimeGraphLoadRecord::locked(&graph_load_record)
            .claim_the_one_load()
            .unwrap();
        let load_outcome = RuntimeGraphLoadRecord::run_the_claimed_load(
            &graph_load_record,
            || Err::<(), _>("the graph holds no node"),
            |load_outcome| load_outcome.map(|()| None).map_err(str::to_owned),
        );
        assert_eq!(load_outcome, Err("the graph holds no node"));
        assert_eq!(
            *RuntimeGraphLoadRecord::locked(&graph_load_record),
            RuntimeGraphLoadRecord::GraphLoadRefused {
                refusal: "the graph holds no node".to_owned()
            }
        );
    }

    #[test]
    fn a_panic_inside_the_claimed_load_is_recorded_as_its_refusal_and_resumes() {
        let graph_load_record = Mutex::new(RuntimeGraphLoadRecord::NoGraphLoaded);
        RuntimeGraphLoadRecord::locked(&graph_load_record)
            .claim_the_one_load()
            .unwrap();

        let unwound = std::panic::catch_unwind(|| {
            RuntimeGraphLoadRecord::run_the_claimed_load(
                &graph_load_record,
                || -> Result<(), String> { panic!("the type resolver fell over") },
                |_load_outcome| Ok(None),
            )
        });

        let panic_payload = unwound.unwrap_err();
        assert_eq!(
            panic_payload.downcast_ref::<&str>(),
            Some(&"the type resolver fell over")
        );
        let record = RuntimeGraphLoadRecord::locked(&graph_load_record);
        assert_eq!(
            *record,
            RuntimeGraphLoadRecord::GraphLoadRefused {
                refusal: "the load panicked: the type resolver fell over".to_owned()
            }
        );
        assert!(
            record
                .refusal_of_run()
                .unwrap()
                .contains("the load panicked: the type resolver fell over")
        );
    }

    /// A Mutex-held record whose one load is claimed and underway.
    fn graph_load_record_with_its_load_claimed() -> Mutex<RuntimeGraphLoadRecord> {
        let graph_load_record = Mutex::new(RuntimeGraphLoadRecord::NoGraphLoaded);
        RuntimeGraphLoadRecord::locked(&graph_load_record)
            .claim_the_one_load()
            .unwrap();
        graph_load_record
    }

    /// Another `load` of the same Runtime, refused because the claimed one is
    /// underway, as a second thread would be.
    fn refuse_another_load_while_underway(graph_load_record: &Mutex<RuntimeGraphLoadRecord>) {
        let refused_while_underway = RuntimeGraphLoadRecord::locked(graph_load_record)
            .claim_the_one_load()
            .unwrap_err();
        assert!(
            refused_while_underway.contains("still underway"),
            "{refused_while_underway}"
        );
    }

    #[test]
    fn a_claimed_loads_own_refusal_stands_over_a_load_refused_while_it_was_underway() {
        let graph_load_record = graph_load_record_with_its_load_claimed();

        let load_outcome = RuntimeGraphLoadRecord::run_the_claimed_load(
            &graph_load_record,
            || {
                refuse_another_load_while_underway(&graph_load_record);
                Err::<(), _>("Unknown processor type `no_such_node`")
            },
            |load_outcome| load_outcome.map(|()| None).map_err(str::to_owned),
        );

        assert_eq!(load_outcome, Err("Unknown processor type `no_such_node`"));
        let record = RuntimeGraphLoadRecord::locked(&graph_load_record);
        assert_eq!(
            *record,
            RuntimeGraphLoadRecord::GraphLoadRefused {
                refusal: "Unknown processor type `no_such_node`".to_owned()
            }
        );
        assert!(
            !record.refusal_of_run().unwrap().contains("still underway"),
            "{:?}",
            record.refusal_of_run()
        );
    }

    #[test]
    fn a_claimed_loads_panic_stands_over_a_load_refused_while_it_was_underway() {
        let graph_load_record = graph_load_record_with_its_load_claimed();

        let unwound = std::panic::catch_unwind(|| {
            RuntimeGraphLoadRecord::run_the_claimed_load(
                &graph_load_record,
                || -> Result<(), String> {
                    refuse_another_load_while_underway(&graph_load_record);
                    panic!("the type resolver fell over")
                },
                |_load_outcome| Ok(None),
            )
        });

        assert!(unwound.is_err());
        assert_eq!(
            *RuntimeGraphLoadRecord::locked(&graph_load_record),
            RuntimeGraphLoadRecord::GraphLoadRefused {
                refusal: "the load panicked: the type resolver fell over".to_owned()
            }
        );
    }

    /// The first refusal while underway stands over the claimed load's
    /// success, and a later refusal does not displace it.
    #[test]
    fn a_load_refused_while_the_claimed_one_was_underway_stands_over_its_success() {
        let graph_load_record = graph_load_record_with_its_load_claimed();

        let load_outcome = RuntimeGraphLoadRecord::run_the_claimed_load(
            &graph_load_record,
            || {
                refuse_another_load_while_underway(&graph_load_record);
                let mut record = RuntimeGraphLoadRecord::locked(&graph_load_record);
                assert!(record.refusal_of_run().unwrap().contains("still underway"));
                record.record_a_refusal_that_claimed_no_load("Runtime.load takes a graph mapping");
                Ok::<_, String>(Some("camera-rig".to_owned()))
            },
            |load_outcome| load_outcome.clone(),
        );

        assert_eq!(load_outcome, Ok(Some("camera-rig".to_owned())));
        let record = RuntimeGraphLoadRecord::locked(&graph_load_record);
        let RuntimeGraphLoadRecord::GraphLoadRefused { refusal } = &*record else {
            panic!("a refusal while underway must stand over a success, got {record:?}");
        };
        assert!(refusal.contains("still underway"), "{refusal}");
        assert!(!refusal.contains("takes a graph mapping"), "{refusal}");
        assert!(record.refusal_of_run().unwrap().contains(refusal.as_str()));
    }
}
