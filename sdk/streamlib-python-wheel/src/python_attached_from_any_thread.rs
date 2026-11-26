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
    // SAFETY: `Py_IsInitialized` may be called at any time; the ensure is paired with exactly one
    // release on this thread, and the `Python` token lives only inside it.
    unsafe {
        if pyo3::ffi::Py_IsInitialized() == 0 {
            return None;
        }
        let interpreter_state_before = pyo3::ffi::PyGILState_Ensure();
        let answered = f(Python::assume_attached());
        pyo3::ffi::PyGILState_Release(interpreter_state_before);
        Some(answered)
    }
}
