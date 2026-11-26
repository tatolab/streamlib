// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Python node's scope: the `streamlib-scope` crate bound for Python. A computed value's build
//! runs on the scope's worker thread, attached to the interpreter only while the build function
//! runs, and a value a build replaces is closed there, off the frame thread.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::thread::ThreadId;
use std::time::Duration;

use crate::python_attached_from_any_thread::attached_from_any_thread;
use pyo3::create_exception;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use streamlib_scope::{
    BuildFailure, Computed, FrameView, Input, LastBuild, ReadRefused, Scope, ScopeBeingDeclared,
    computed, fail_this_build_on_a_computed_value_not_yet_built,
};

#[cfg(test)]
mod tests;

create_exception!(
    tatolab.runtime._engine,
    NodeScopeReadOutsideABuildOrAFrame,
    PyRuntimeError,
    "`.get()` reads only in a computed value's build or in the per-frame function."
);
create_exception!(
    tatolab.runtime._engine,
    NodeScopeHandleOfAnotherScope,
    PyRuntimeError,
    "The handle belongs to another node's scope than the build or frame reading it."
);
create_exception!(
    tatolab.runtime._engine,
    NodeScopeComputedValueNotYetBuilt,
    PyRuntimeError,
    "A build read a computed value that has no value yet; it builds again once that value is built."
);

fn raise_the_read_refusal(refused: ReadRefused) -> PyErr {
    let why = refused.to_string();
    match refused {
        ReadRefused::OutsideABuildOrAFrame => NodeScopeReadOutsideABuildOrAFrame::new_err(why),
        ReadRefused::HandleOfAnotherScope => NodeScopeHandleOfAnotherScope::new_err(why),
        ReadRefused::ComputedValueNotYetBuilt => NodeScopeComputedValueNotYetBuilt::new_err(why),
    }
}

/// What holds one Python object in the scopes of this process.
#[derive(Default)]
struct HoldersOfAPythonObject {
    holders: usize,
    held_by_an_input: bool,
}

/// The holders of each Python object the scopes of this process hold, by its address, so an
/// object is closed only once nothing holds it, and never when it came in through an input.
static HOLDERS_OF_EACH_PYTHON_OBJECT: LazyLock<
    parking_lot::Mutex<HashMap<usize, HoldersOfAPythonObject>>,
> = LazyLock::new(Default::default);

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeldAs {
    AnInputValue,
    AComputedValue,
}

/// A Python object held in a node's scope: an input's value or a computed value. Two values are
/// equal when they are one object or `==` says so; an `==` that raises counts as a change. A
/// computed value is closed when the last holder of its object lets it go.
struct PythonValueInANodeScope {
    object: Option<Py<PyAny>>,
    held_as: HeldAs,
}

impl PythonValueInANodeScope {
    fn held_as(object: Py<PyAny>, held_as: HeldAs) -> Self {
        let mut holders = HOLDERS_OF_EACH_PYTHON_OBJECT.lock();
        let holders_of_the_object = holders.entry(object.as_ptr() as usize).or_default();
        holders_of_the_object.holders += 1;
        holders_of_the_object.held_by_an_input |= held_as == HeldAs::AnInputValue;
        drop(holders);
        Self {
            object: Some(object),
            held_as,
        }
    }

    fn an_input_value(object: Py<PyAny>) -> Self {
        Self::held_as(object, HeldAs::AnInputValue)
    }

    fn a_computed_value(object: Py<PyAny>) -> Self {
        Self::held_as(object, HeldAs::AComputedValue)
    }

    fn object(&self) -> &Py<PyAny> {
        self.object
            .as_ref()
            .expect("a value holds its object until it is dropped")
    }
}

impl PartialEq for PythonValueInANodeScope {
    fn eq(&self, other: &Self) -> bool {
        attached_from_any_thread(|python| {
            let (left, right) = (self.object().bind(python), other.object().bind(python));
            left.is(right) || left.eq(right).unwrap_or(false)
        })
        .unwrap_or(false)
    }
}

impl Drop for PythonValueInANodeScope {
    fn drop(&mut self) {
        let Some(object) = self.object.take() else {
            return;
        };
        let held_as = self.held_as;
        attached_from_any_thread(|python| {
            if is_closed_as_its_last_holder_lets_go(&object, held_as) {
                close_the_computed_value(python, object.bind(python));
            }
            drop(object);
        });
    }
}

fn is_closed_as_its_last_holder_lets_go(object: &Py<PyAny>, held_as: HeldAs) -> bool {
    let mut holders = HOLDERS_OF_EACH_PYTHON_OBJECT.lock();
    let address = object.as_ptr() as usize;
    let Some(holders_of_the_object) = holders.get_mut(&address) else {
        return false;
    };
    holders_of_the_object.holders -= 1;
    if holders_of_the_object.holders > 0 {
        return false;
    }
    let held_by_an_input = holders_of_the_object.held_by_an_input;
    holders.remove(&address);
    held_as == HeldAs::AComputedValue && !held_by_an_input
}

fn close_the_computed_value(python: Python<'_>, value: &Bound<'_, PyAny>) {
    let Ok(close) = value.getattr("close") else {
        return;
    };
    if let Err(raised) = close.call0() {
        raised.write_unraisable(python, Some(value));
    }
}

/// How a call of a computed value's Python build function ended.
enum PythonBuildEnded {
    Built(PythonValueInANodeScope),
    ReadAComputedValueNotYetBuilt,
    Raised(String),
}

/// Build a computed value by calling `build`, attached to the interpreter only while it runs.
fn build_with_the_python_function(build: &Py<PyAny>) -> Result<PythonValueInANodeScope, String> {
    let ended = Python::attach(|python| match build.bind(python).call0() {
        Ok(value) => {
            PythonBuildEnded::Built(PythonValueInANodeScope::a_computed_value(value.unbind()))
        }
        Err(raised) if raised.is_instance_of::<NodeScopeComputedValueNotYetBuilt>(python) => {
            PythonBuildEnded::ReadAComputedValueNotYetBuilt
        }
        Err(raised) => PythonBuildEnded::Raised(raised.to_string()),
    });
    match ended {
        PythonBuildEnded::Built(value) => Ok(value),
        PythonBuildEnded::ReadAComputedValueNotYetBuilt => {
            fail_this_build_on_a_computed_value_not_yet_built()
        }
        PythonBuildEnded::Raised(why) => Err(why),
    }
}

/// An input of a Python node's scope, read with `get()` in a build or a frame.
#[pyclass(frozen, name = "NodeScopeInput", module = "tatolab.runtime._engine")]
pub(crate) struct PythonNodeScopeInput(Input<PythonValueInANodeScope>);

#[pymethods]
impl PythonNodeScopeInput {
    /// The input's value: in a build, as the build started, recorded as read; in a frame, as of
    /// the frame's boundary.
    fn get(&self, python: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = self.0.try_get().map_err(raise_the_read_refusal)?;
        Ok(value.object().clone_ref(python))
    }
}

/// A value computed from a Python node's inputs, read with `get()` in a build or a frame.
#[pyclass(
    frozen,
    name = "NodeScopeComputedValue",
    module = "tatolab.runtime._engine"
)]
pub(crate) struct PythonNodeScopeComputedValue(Computed<PythonValueInANodeScope>);

#[pymethods]
impl PythonNodeScopeComputedValue {
    /// The computed value: in a build, as the build started, recorded as read; in a frame,
    /// always a value.
    fn get(&self, python: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = self.0.try_get().map_err(raise_the_read_refusal)?;
        Ok(value.object().clone_ref(python))
    }
}

/// Declare a value `build` computes from what it reads with `get()`, into the scope being
/// declared on this thread.
#[pyfunction(name = "computed")]
pub(crate) fn declare_a_computed_value_of_a_python_node(
    build: Py<PyAny>,
) -> PythonNodeScopeComputedValue {
    PythonNodeScopeComputedValue(computed(move || build_with_the_python_function(&build)))
}

/// A Python node's scope while its body declares it: its inputs, then its computed values.
#[pyclass(name = "NodeScopeBeingDeclared", module = "tatolab.runtime._engine")]
pub(crate) struct PythonNodeScopeBeingDeclared(Option<ScopeBeingDeclared>);

impl PythonNodeScopeBeingDeclared {
    fn being_declared(&mut self) -> PyResult<&mut ScopeBeingDeclared> {
        self.0.as_mut().ok_or_else(refuse_a_started_scope)
    }
}

fn refuse_a_started_scope() -> PyErr {
    PyRuntimeError::new_err("the node's scope has already started")
}

#[pymethods]
impl PythonNodeScopeBeingDeclared {
    #[new]
    fn new() -> Self {
        Self(Some(ScopeBeingDeclared::new()))
    }

    /// An input holding `value` until it is written.
    fn input(&mut self, value: Py<PyAny>) -> PyResult<PythonNodeScopeInput> {
        Ok(PythonNodeScopeInput(
            self.being_declared()?
                .input(PythonValueInANodeScope::an_input_value(value)),
        ))
    }

    /// Call the node's body once, with every `computed` it calls declared into this scope, and
    /// answer what it returned.
    fn declare(&mut self, python: Python<'_>, body: Py<PyAny>) -> PyResult<Py<PyAny>> {
        self.being_declared()?
            .declare(|| body.bind(python).call0().map(Bound::unbind))
    }

    /// Start the scope's worker, which builds every computed value from here on.
    fn start(&mut self) -> PyResult<PythonRunningNodeScope> {
        let declared = self.0.take().ok_or_else(refuse_a_started_scope)?;
        let (scope, frame_view) = declared.start().map_err(|error| {
            PyRuntimeError::new_err(format!("the node's scope worker did not start: {error}"))
        })?;
        Ok(PythonRunningNodeScope {
            scope: parking_lot::RwLock::new(Some(scope)),
            frame_view: parking_lot::Mutex::new(Some(frame_view)),
            thread_running_a_frame: parking_lot::Mutex::new(None),
        })
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
    /// Of a failed build: `"raised"`, `"read_a_computed_value_not_yet_built"` or `"panicked"`.
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
                }),
                why: Some(why.to_string()),
            },
        }
    }
}

/// A Python node's running scope: sets and wires write its inputs, and the frame thread runs
/// each frame through it.
///
/// Every wait on its locks is made detached from the interpreter, since the thread holding a lock
/// may need the interpreter to let go: a frame runs Python holding `frame_view`, and the scope's
/// worker compares two values holding the scope's own lock. `frame_view` is taken before `scope`.
#[pyclass(frozen, name = "RunningNodeScope", module = "tatolab.runtime._engine")]
pub(crate) struct PythonRunningNodeScope {
    scope: parking_lot::RwLock<Option<Scope>>,
    frame_view: parking_lot::Mutex<Option<FrameView>>,
    thread_running_a_frame: parking_lot::Mutex<Option<ThreadId>>,
}

fn refuse_a_closed_scope() -> PyErr {
    PyRuntimeError::new_err("the node's scope is closed")
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

impl PythonRunningNodeScope {
    /// Run `with_the_scope` on the running scope, detached from the interpreter.
    fn with_the_scope_detached<R: Send>(
        &self,
        python: Python<'_>,
        with_the_scope: impl FnOnce(&Scope) -> R + Send,
    ) -> PyResult<R> {
        python
            .detach(|| self.scope.read().as_ref().map(with_the_scope))
            .ok_or_else(refuse_a_closed_scope)
    }
}

fn drop_the_scope_detached(
    python: Python<'_>,
    scope: Option<Scope>,
    frame_view: Option<FrameView>,
) {
    // The worker may be waiting to attach for its build in flight, and closing a computed value
    // attaches on its own.
    python.detach(|| drop((frame_view, scope)));
}

#[pymethods]
impl PythonRunningNodeScope {
    /// Write `value` to `input`; a value equal to the one it holds builds nothing.
    fn write(
        &self,
        python: Python<'_>,
        input: &PythonNodeScopeInput,
        value: Py<PyAny>,
    ) -> PyResult<()> {
        let value = PythonValueInANodeScope::an_input_value(value);
        self.with_the_scope_detached(python, |scope| scope.write(input.0, value))
    }

    /// Take the values built since the last frame, then call `frame` with `get()` reading them,
    /// once every computed value has its first value; whether `frame` ran.
    fn run_frame(&self, python: Python<'_>, frame: Py<PyAny>) -> PyResult<bool> {
        let mut frame_view = self.frame_view.try_lock().ok_or_else(|| {
            PyRuntimeError::new_err("a frame of this node's scope is already running")
        })?;
        let frame_view = frame_view.as_mut().ok_or_else(refuse_a_closed_scope)?;
        let _frame_running = FrameRunningOnThisThread::mark(&self.thread_running_a_frame);
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
        Ok(self
            .with_the_scope_detached(python, |scope| scope.last_build_of(computed_value.0))?
            .into())
    }

    /// Wait up to `timeout_seconds` until no computed value is left to build; whether it settled.
    fn wait_until_settled(&self, python: Python<'_>, timeout_seconds: f64) -> PyResult<bool> {
        let timeout = Duration::try_from_secs_f64(timeout_seconds)
            .map_err(|error| PyValueError::new_err(format!("timeout_seconds: {error}")))?;
        self.with_the_scope_detached(python, |scope| scope.wait_until_settled(timeout))
    }

    /// Wait for a frame running on another thread, stop the worker once its build in flight
    /// returns, and close every computed value. Refused inside a frame of this scope.
    fn close(&self, python: Python<'_>) -> PyResult<()> {
        if *self.thread_running_a_frame.lock() == Some(std::thread::current().id()) {
            return Err(PyRuntimeError::new_err(
                "a frame cannot close its own node's scope; close it once the frame returns",
            ));
        }
        let (scope, frame_view) = python.detach(|| {
            let frame_view = self.frame_view.lock().take();
            (self.scope.write().take(), frame_view)
        });
        drop_the_scope_detached(python, scope, frame_view);
        Ok(())
    }
}

impl Drop for PythonRunningNodeScope {
    fn drop(&mut self) {
        let mut scope = self.scope.get_mut().take();
        let mut frame_view = self.frame_view.get_mut().take();
        Python::try_attach(|python| {
            drop_the_scope_detached(python, scope.take(), frame_view.take());
        });
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
        "NodeScopeHandleOfAnotherScope",
        python.get_type::<NodeScopeHandleOfAnotherScope>(),
    )?;
    module.add(
        "NodeScopeComputedValueNotYetBuilt",
        python.get_type::<NodeScopeComputedValueNotYetBuilt>(),
    )?;
    Ok(())
}
