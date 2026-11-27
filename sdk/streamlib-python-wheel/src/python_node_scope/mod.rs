// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Python node's scope: the `streamlib-scope` crate bound for Python. A computed value's build
//! runs on the scope's worker thread, attached to the interpreter only while the build function
//! runs, and a value a build replaces is closed there, off the frame thread.

mod calls_refused_in_a_frame;
mod python_value_in_a_node_scope;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use pyo3::create_exception;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use streamlib_scope::{
    BuildFailure, Computed, FrameView, Input, LastBuild, ReadRefused, Scope, ScopeBeingDeclared,
    computed, fail_this_build_on_a_computed_value_not_yet_built,
};

use crate::python_attached_from_any_thread::attached_from_any_thread;
pub(crate) use calls_refused_in_a_frame::{raised_while_a_frame_runs, worded_while_a_frame_runs};
use python_value_in_a_node_scope::PythonValueInANodeScope;

create_exception!(
    tatolab.runtime._engine,
    NodeScopeReadOutsideABuildOrAFrame,
    PyRuntimeError,
    "`.get()` reads only in a computed value's build or in the per-frame function."
);
create_exception!(
    tatolab.runtime._engine,
    NodeScopeReadInTheBody,
    PyRuntimeError,
    "`.get()` can't read in the node's body, which runs once; read it in a computed value's build or in the per-frame function."
);
create_exception!(
    tatolab.runtime._engine,
    NodeScopeHandleOfAnotherScope,
    PyRuntimeError,
    "The handle belongs to another node's scope than the one it was handed to."
);
create_exception!(
    tatolab.runtime._engine,
    NodeScopeComputedValueNotYetBuilt,
    PyRuntimeError,
    "A build read a computed value that has no value yet; it builds again once that value is built."
);
create_exception!(
    tatolab.runtime._engine,
    NodeScopeComputedOutsideADeclaration,
    PyRuntimeError,
    "`computed` declares only while a node's body declares its scope."
);

fn raise_the_read_refusal(refused: ReadRefused) -> PyErr {
    let why = refused.to_string();
    match refused {
        ReadRefused::OutsideABuildOrAFrame => NodeScopeReadOutsideABuildOrAFrame::new_err(why),
        ReadRefused::InTheBody => NodeScopeReadInTheBody::new_err(why),
        ReadRefused::HandleOfAnotherScope => NodeScopeHandleOfAnotherScope::new_err(why),
        ReadRefused::ComputedValueNotYetBuilt => NodeScopeComputedValueNotYetBuilt::new_err(why),
    }
}

fn refuse_a_handle_of_another_scope() -> PyErr {
    NodeScopeHandleOfAnotherScope::new_err("the handle belongs to another node's scope")
}

fn refuse_a_started_scope() -> PyErr {
    PyRuntimeError::new_err("the node's scope has already started")
}

fn refuse_a_closed_scope() -> PyErr {
    PyRuntimeError::new_err("the node's scope is closed")
}

/// Which of this process's node scopes a handle was declared in.
#[derive(Clone, Copy, PartialEq, Eq)]
struct NodeScopeIdentity(u64);

impl NodeScopeIdentity {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The thread a scope's builds run on, known once its first build runs.
type WorkerThreadOfAScope = Arc<OnceLock<ThreadId>>;

/// The scope a node's body declares on this thread, for `computed` to declare into.
#[derive(Clone)]
struct NodeScopeDeclaredOnThisThread {
    identity: NodeScopeIdentity,
    worker_thread: WorkerThreadOfAScope,
}

thread_local! {
    static NODE_SCOPE_DECLARED_ON_THIS_THREAD: RefCell<Option<NodeScopeDeclaredOnThisThread>> =
        const { RefCell::new(None) };
}

/// Puts back the scope declared on this thread before a body ran, also when the body raises.
struct DeclaringOnThisThread(Option<NodeScopeDeclaredOnThisThread>);

impl Drop for DeclaringOnThisThread {
    fn drop(&mut self) {
        NODE_SCOPE_DECLARED_ON_THIS_THREAD.set(self.0.take());
    }
}

/// How a call of a computed value's Python build function ended.
enum PythonBuildEnded {
    Built(PythonValueInANodeScope),
    ReadAComputedValueNotYetBuilt,
    Raised(String),
    InterpreterGone,
}

/// Build a computed value by calling `build`, attached to the interpreter only while it runs.
/// A computed value's Python build function, released attached wherever the scope lets go of it.
struct PythonBuildFunction(Option<Py<PyAny>>);

impl Drop for PythonBuildFunction {
    fn drop(&mut self) {
        if let Some(build) = self.0.take() {
            attached_from_any_thread(|python| drop(build.into_bound(python)));
        }
    }
}

fn build_with_the_python_function(
    build: &PythonBuildFunction,
) -> Result<PythonValueInANodeScope, String> {
    let build = build
        .0
        .as_ref()
        .expect("a build function is held until the scope lets go of it");
    let ended = Python::try_attach(|python| match build.bind(python).call0() {
        Ok(value) => {
            PythonBuildEnded::Built(PythonValueInANodeScope::a_computed_value(value.unbind()))
        }
        Err(raised) if raised.is_instance_of::<NodeScopeComputedValueNotYetBuilt>(python) => {
            PythonBuildEnded::ReadAComputedValueNotYetBuilt
        }
        Err(raised) => PythonBuildEnded::Raised(raised.to_string()),
    })
    .unwrap_or(PythonBuildEnded::InterpreterGone);
    match ended {
        PythonBuildEnded::Built(value) => Ok(value),
        PythonBuildEnded::ReadAComputedValueNotYetBuilt => {
            fail_this_build_on_a_computed_value_not_yet_built()
        }
        PythonBuildEnded::Raised(why) => Err(why),
        PythonBuildEnded::InterpreterGone => {
            Err("the Python interpreter is shutting down".to_string())
        }
    }
}

/// An input of a Python node's scope, read with `get()` in a build or a frame.
#[pyclass(frozen, name = "NodeScopeInput", module = "tatolab.runtime._engine")]
pub(crate) struct PythonNodeScopeInput {
    declared_in: NodeScopeIdentity,
    input: Input<PythonValueInANodeScope>,
}

#[pymethods]
impl PythonNodeScopeInput {
    /// The input's value: in a build, as the build started, recorded as read; in a frame, as of
    /// the frame's boundary.
    fn get(&self, python: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = self.input.try_get().map_err(raise_the_read_refusal)?;
        Ok(value.object().clone_ref(python))
    }
}

/// A value computed from a Python node's inputs, read with `get()` in a build or a frame.
#[pyclass(frozen, name = "NodeScopeComputedValue", module = "tatolab.stream")]
pub(crate) struct PythonNodeScopeComputedValue {
    declared_in: NodeScopeIdentity,
    computed: Computed<PythonValueInANodeScope>,
}

#[pymethods]
impl PythonNodeScopeComputedValue {
    /// The computed value: in a build, as the build started, recorded as read; in a frame,
    /// always a value.
    fn get(&self, python: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = self.computed.try_get().map_err(raise_the_read_refusal)?;
        Ok(value.object().clone_ref(python))
    }
}

/// Declare a value `build` computes from what it reads with `get()`, into the scope a node's body
/// declares on this thread.
#[pyfunction(name = "computed")]
pub(crate) fn declare_a_computed_value_of_a_python_node(
    build: Py<PyAny>,
) -> PyResult<PythonNodeScopeComputedValue> {
    let Some(declared) = NODE_SCOPE_DECLARED_ON_THIS_THREAD.with_borrow(Clone::clone) else {
        return Err(NodeScopeComputedOutsideADeclaration::new_err(
            "`computed` declares only while a node's body declares its scope; call it in the body",
        ));
    };
    let worker_thread = declared.worker_thread;
    let build = PythonBuildFunction(Some(build));
    Ok(PythonNodeScopeComputedValue {
        declared_in: declared.identity,
        computed: computed(move || {
            worker_thread.get_or_init(|| std::thread::current().id());
            build_with_the_python_function(&build)
        }),
    })
}

/// A Python node's scope while its body declares it: its inputs, then its computed values.
#[pyclass(name = "NodeScopeBeingDeclared", module = "tatolab.runtime._engine")]
pub(crate) struct PythonNodeScopeBeingDeclared {
    declared: Option<ScopeBeingDeclared>,
    on_this_thread: NodeScopeDeclaredOnThisThread,
}

impl PythonNodeScopeBeingDeclared {
    fn being_declared(&mut self) -> PyResult<&mut ScopeBeingDeclared> {
        self.declared.as_mut().ok_or_else(refuse_a_started_scope)
    }
}

#[pymethods]
impl PythonNodeScopeBeingDeclared {
    #[new]
    fn new() -> Self {
        Self {
            declared: Some(ScopeBeingDeclared::new()),
            on_this_thread: NodeScopeDeclaredOnThisThread {
                identity: NodeScopeIdentity::next(),
                worker_thread: Arc::default(),
            },
        }
    }

    /// An input holding `value` until it is written.
    fn input(&mut self, value: Py<PyAny>) -> PyResult<PythonNodeScopeInput> {
        Ok(PythonNodeScopeInput {
            declared_in: self.on_this_thread.identity,
            input: self
                .being_declared()?
                .input(PythonValueInANodeScope::an_input_value(value)),
        })
    }

    /// Call the node's body once, with every `computed` it calls declared into this scope, and
    /// answer what it returned.
    fn declare(&mut self, python: Python<'_>, body: Py<PyAny>) -> PyResult<Py<PyAny>> {
        let on_this_thread = self.on_this_thread.clone();
        let declared = self.being_declared()?;
        let _declaring =
            DeclaringOnThisThread(NODE_SCOPE_DECLARED_ON_THIS_THREAD.replace(Some(on_this_thread)));
        declared.declare(|| body.bind(python).call0().map(Bound::unbind))
    }

    /// Start the scope's worker, which builds every computed value from here on.
    fn start(&mut self, python: Python<'_>) -> PyResult<PythonRunningNodeScope> {
        let declared = self.declared.take().ok_or_else(refuse_a_started_scope)?;
        let (scope, frame_view) = declared.start().map_err(|error| {
            PyRuntimeError::new_err(format!("the node's scope worker did not start: {error}"))
        })?;
        let running = Arc::new(RunningNodeScopeShared {
            identity: self.on_this_thread.identity,
            worker_thread: Arc::clone(&self.on_this_thread.worker_thread),
            scope: parking_lot::RwLock::new(Some(scope)),
            frame_view: parking_lot::Mutex::new(Some(frame_view)),
            thread_running_a_frame: parking_lot::Mutex::new(None),
            letting_go: parking_lot::ReentrantMutex::new(()),
        });
        close_at_interpreter_exit(python, &running)?;
        Ok(PythonRunningNodeScope(running))
    }
}

/// How the last build of a computed value went, for a node's diagnostics.
#[pyclass(
    frozen,
    get_all,
    name = "LastBuildOfANodeScopeComputedValue",
    module = "tatolab.runtime._engine"
)]
pub(crate) struct PythonLastBuildOfANodeScopeComputedValue {
    /// `"not_yet_built"`, `"built"` or `"failed"`.
    state: &'static str,
    /// How many builds have run.
    builds: u64,
    /// Of a failed build: `"raised"`, `"read_a_computed_value_not_yet_built"`, `"panicked"` or
    /// `"comparison_panicked"`.
    failure: Option<&'static str>,
    /// Of a failed build: why, as the exception or the panic said it.
    why: Option<String>,
}

impl From<LastBuild> for PythonLastBuildOfANodeScopeComputedValue {
    fn from(last_build: LastBuild) -> Self {
        match last_build {
            LastBuild::NotYetBuilt => Self {
                state: "not_yet_built",
                builds: 0,
                failure: None,
                why: None,
            },
            LastBuild::Built { builds } => Self {
                state: "built",
                builds,
                failure: None,
                why: None,
            },
            LastBuild::Failed { builds, why } => Self {
                state: "failed",
                builds,
                failure: Some(match why {
                    BuildFailure::Returned(_) => "raised",
                    BuildFailure::ReadAComputedValueNotYetBuilt => {
                        "read_a_computed_value_not_yet_built"
                    }
                    BuildFailure::Panicked(_) => "panicked",
                    BuildFailure::ComparisonPanicked(_) => "comparison_panicked",
                }),
                why: Some(why.to_string()),
            },
        }
    }
}

/// A running scope, shared by its Python object and the close made at interpreter exit.
///
/// Every wait on its locks is made detached from the interpreter, since the thread holding a lock
/// may need the interpreter to let go: a frame runs Python holding `frame_view`, and the scope's
/// worker compares two values holding the scope's own lock.
struct RunningNodeScopeShared {
    identity: NodeScopeIdentity,
    worker_thread: WorkerThreadOfAScope,
    scope: parking_lot::RwLock<Option<Scope>>,
    frame_view: parking_lot::Mutex<Option<FrameView>>,
    thread_running_a_frame: parking_lot::Mutex<Option<ThreadId>>,
    /// Reentrant: a value's `close()` run while letting go may close or drop its own scope.
    letting_go: parking_lot::ReentrantMutex<()>,
}

impl RunningNodeScopeShared {
    /// Wait for a frame running on another thread, stop the worker once its build in flight
    /// returns, and drop every value, closing what a build made. Made detached, never on the
    /// scope's worker or inside one of its frames.
    /// Every caller waits until the first has let go, so a second close, or the one at
    /// interpreter exit, returns only once the worker is joined and the values are closed.
    fn let_go_of_the_scope(&self) {
        let _letting_go = self.letting_go.lock();
        let frame_view = self.frame_view.lock().take();
        let scope = self.scope.write().take();
        drop(scope);
        drop(frame_view);
    }

    fn is_the_worker_thread(&self) -> bool {
        self.worker_thread.get() == Some(&std::thread::current().id())
    }
}

/// The running scopes of this process, closed at interpreter exit while the interpreter can still
/// run their workers' builds and their values' `close()`.
static RUNNING_NODE_SCOPES: parking_lot::Mutex<Vec<Weak<RunningNodeScopeShared>>> =
    parking_lot::Mutex::new(Vec::new());

fn close_at_interpreter_exit(
    python: Python<'_>,
    running: &Arc<RunningNodeScopeShared>,
) -> PyResult<()> {
    static CLOSE_REGISTERED_AT_EXIT: AtomicBool = AtomicBool::new(false);
    {
        let mut running_scopes = RUNNING_NODE_SCOPES.lock();
        running_scopes.retain(|running| running.strong_count() > 0);
        running_scopes.push(Arc::downgrade(running));
    }
    if !CLOSE_REGISTERED_AT_EXIT.swap(true, Ordering::SeqCst) {
        let close_every_node_scope_left_open =
            wrap_pyfunction!(close_every_node_scope_left_open, python)?;
        python
            .import("atexit")?
            .call_method1("register", (close_every_node_scope_left_open,))?;
    }
    Ok(())
}

#[pyfunction]
fn close_every_node_scope_left_open(python: Python<'_>) {
    let running_scopes: Vec<_> = std::mem::take(&mut *RUNNING_NODE_SCOPES.lock());
    for running in running_scopes.iter().filter_map(Weak::upgrade) {
        python.detach(|| running.let_go_of_the_scope());
    }
}

/// Marks the thread a frame runs on until the frame returns or raises.
struct FrameRunningOnThisThread<'scope>(&'scope parking_lot::Mutex<Option<ThreadId>>);

impl<'scope> FrameRunningOnThisThread<'scope> {
    fn mark(thread_running_a_frame: &'scope parking_lot::Mutex<Option<ThreadId>>) -> Self {
        *thread_running_a_frame.lock() = Some(std::thread::current().id());
        Self(thread_running_a_frame)
    }
}

impl Drop for FrameRunningOnThisThread<'_> {
    fn drop(&mut self) {
        *self.0.lock() = None;
    }
}

/// A Python node's running scope: sets and wires write its inputs, and the frame thread runs
/// each frame through it. Close it with `close()`: the cycle a build function can make back to
/// the node holding it is not one the garbage collector can see.
#[pyclass(frozen, name = "RunningNodeScope", module = "tatolab.runtime._engine")]
pub(crate) struct PythonRunningNodeScope(Arc<RunningNodeScopeShared>);

impl PythonRunningNodeScope {
    /// Run `with_the_scope` on the running scope, detached from the interpreter.
    fn with_the_scope_detached<R: Send>(
        &self,
        python: Python<'_>,
        with_the_scope: impl FnOnce(&Scope) -> R + Send,
    ) -> PyResult<R> {
        python
            .detach(|| self.0.scope.read().as_ref().map(with_the_scope))
            .ok_or_else(refuse_a_closed_scope)
    }

    fn refuse_a_handle_declared_elsewhere(&self, declared_in: NodeScopeIdentity) -> PyResult<()> {
        if declared_in == self.0.identity {
            Ok(())
        } else {
            Err(refuse_a_handle_of_another_scope())
        }
    }
}

#[pymethods]
impl PythonRunningNodeScope {
    /// Write each `(input, value)` of `writes` in one write, so a build or a frame sees all of
    /// them or none, and a computed value reading several builds once; a value equal to the one
    /// its input holds builds nothing.
    fn write(
        &self,
        python: Python<'_>,
        writes: Vec<(PyRef<'_, PythonNodeScopeInput>, Py<PyAny>)>,
    ) -> PyResult<()> {
        let writes = writes
            .into_iter()
            .map(|(input, value)| {
                self.refuse_a_handle_declared_elsewhere(input.declared_in)?;
                Ok((input.input, PythonValueInANodeScope::an_input_value(value)))
            })
            .collect::<PyResult<Vec<_>>>()?;
        self.with_the_scope_detached(python, |scope| {
            scope.write(|written| {
                for (input, value) in writes {
                    written.write(input, value);
                }
            });
        })
    }

    /// Take the values built since the last frame, then call `frame` with `get()` reading them,
    /// once every computed value has its first value; whether `frame` ran.
    fn run_frame(&self, python: Python<'_>, frame: Py<PyAny>) -> PyResult<bool> {
        let mut frame_view = self.0.frame_view.try_lock().ok_or_else(|| {
            PyRuntimeError::new_err("a frame of this node's scope is already running")
        })?;
        let frame_view = frame_view.as_mut().ok_or_else(refuse_a_closed_scope)?;
        let _frame_running = FrameRunningOnThisThread::mark(&self.0.thread_running_a_frame);
        match frame_view.run_frame(|| frame.bind(python).call0()) {
            None => Ok(false),
            Some(ran) => ran.map(|_| true),
        }
    }

    /// How the last build of `computed_value` went.
    fn last_build_of(
        &self,
        python: Python<'_>,
        computed_value: &PythonNodeScopeComputedValue,
    ) -> PyResult<PythonLastBuildOfANodeScopeComputedValue> {
        self.refuse_a_handle_declared_elsewhere(computed_value.declared_in)?;
        Ok(self
            .with_the_scope_detached(python, |scope| scope.last_build_of(computed_value.computed))?
            .into())
    }

    /// Wait up to `timeout_seconds` until no computed value is left to build; whether it settled.
    fn wait_until_settled(&self, python: Python<'_>, timeout_seconds: f64) -> PyResult<bool> {
        let timeout = Duration::try_from_secs_f64(timeout_seconds)
            .map_err(|error| PyValueError::new_err(format!("timeout_seconds: {error}")))?;
        let deadline = Instant::now() + timeout;
        // Checked once per hold of the scope, so a close or a write never waits out the timeout.
        loop {
            if self
                .with_the_scope_detached(python, |scope| scope.wait_until_settled(Duration::ZERO))?
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            python.detach(|| std::thread::sleep(Duration::from_millis(1)));
        }
    }

    /// Wait for a frame running on another thread, stop the worker once its build in flight
    /// returns, and close every value a build made. Refused inside a frame or a build of this
    /// scope.
    fn close(&self, python: Python<'_>) -> PyResult<()> {
        if *self.0.thread_running_a_frame.lock() == Some(std::thread::current().id()) {
            return Err(PyRuntimeError::new_err(
                "a frame cannot close its own node's scope; close it once the frame returns",
            ));
        }
        if self.0.is_the_worker_thread() {
            return Err(PyRuntimeError::new_err(
                "a build cannot close its own node's scope; close it from the node's thread",
            ));
        }
        python.detach(|| self.0.let_go_of_the_scope());
        Ok(())
    }
}

impl Drop for PythonRunningNodeScope {
    fn drop(&mut self) {
        let running = Arc::clone(&self.0);
        if running.is_the_worker_thread() {
            // The worker cannot join itself: another thread lets go once the build returns.
            let let_go_elsewhere = std::thread::Builder::new()
                .name("scope-let-go".into())
                .spawn(move || running.let_go_of_the_scope());
            if let Err(error) = let_go_elsewhere {
                tracing::error!(%error, "a node scope dropped by its own build could not be let go");
            }
            return;
        }
        let let_go_detached = Python::try_attach(|python| {
            python.detach(|| running.let_go_of_the_scope());
        });
        if let_go_detached.is_none() {
            self.0.let_go_of_the_scope();
        }
    }
}

pub(crate) fn add_the_node_scope_to(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let python = module.py();
    module.add_class::<PythonNodeScopeBeingDeclared>()?;
    module.add_class::<PythonNodeScopeInput>()?;
    module.add_class::<PythonNodeScopeComputedValue>()?;
    module.add_class::<PythonRunningNodeScope>()?;
    module.add_class::<PythonLastBuildOfANodeScopeComputedValue>()?;
    module.add_function(wrap_pyfunction!(
        declare_a_computed_value_of_a_python_node,
        module
    )?)?;
    module.add(
        "NodeScopeReadOutsideABuildOrAFrame",
        python.get_type::<NodeScopeReadOutsideABuildOrAFrame>(),
    )?;
    module.add(
        "NodeScopeReadInTheBody",
        python.get_type::<NodeScopeReadInTheBody>(),
    )?;
    module.add(
        "NodeScopeHandleOfAnotherScope",
        python.get_type::<NodeScopeHandleOfAnotherScope>(),
    )?;
    module.add(
        "NodeScopeComputedValueNotYetBuilt",
        python.get_type::<NodeScopeComputedValueNotYetBuilt>(),
    )?;
    module.add(
        "NodeScopeComputedOutsideADeclaration",
        python.get_type::<NodeScopeComputedOutsideADeclaration>(),
    )?;
    calls_refused_in_a_frame::add_the_calls_refused_in_a_frame_to(module)
}
