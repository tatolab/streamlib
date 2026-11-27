// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Running Python from a `Drop` or a comparison that can happen on any thread.

use pyo3::Python;

/// Run `f` attached to the interpreter, from any thread; once the interpreter is gone, `f` is
/// dropped uncalled, with whatever it captured.
///
/// A value can be dropped on a thread pyo3 still counts as attached inside a native call that
/// released the thread state (torch frees a tensor inside its own `gil_scoped_release`), so a bare
/// `Python::attach` there would run Python with no thread state and abort the interpreter.
/// `PyGILState_Ensure` asks CPython itself, so it reacquires a released thread state, nests under
/// a held one, and creates one on a foreign thread.
pub(crate) fn attached_from_any_thread<R>(f: impl FnOnce(Python<'_>) -> R) -> Option<R> {
    // SAFETY: `Py_IsInitialized` may be called at any time.
    if unsafe { pyo3::ffi::Py_IsInitialized() } == 0 {
        return None;
    }
    let _attached = InterpreterAttachedOnThisThread::ensure();
    // Attached through pyo3 as well, so a reference `f` drops, a `PyErr`'s included, is released
    // here rather than deferred to whichever thread next attaches, the frame thread included.
    let mut f = Some(f);
    if let Some(answer) = Python::try_attach(|python| f.take().map(|f| f(python))) {
        return answer;
    }
    // pyo3 refuses during a GC traversal or the interpreter's shutdown; the ensure still holds.
    // SAFETY: the interpreter is attached on this thread until `_attached` drops, after `f`.
    f.map(|f| f(unsafe { Python::assume_attached() }))
}

/// One `PyGILState_Ensure`, released once on drop, also when `f` unwinds.
struct InterpreterAttachedOnThisThread(pyo3::ffi::PyGILState_STATE);

impl InterpreterAttachedOnThisThread {
    fn ensure() -> Self {
        // SAFETY: the interpreter is initialized; the ensure is paired with the release in `drop`.
        Self(unsafe { pyo3::ffi::PyGILState_Ensure() })
    }
}

impl Drop for InterpreterAttachedOnThisThread {
    fn drop(&mut self) {
        // SAFETY: releases the one ensure `ensure` made on this thread.
        unsafe { pyo3::ffi::PyGILState_Release(self.0) };
    }
}

#[cfg(test)]
mod tests;
