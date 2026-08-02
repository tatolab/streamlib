// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Process-global slot mapping a config token to the Python callable a
//! [`PythonCallbackStageProcessor`] invokes per frame.
//!
//! [`PythonCallbackStageProcessor`]: crate::python_callback_stage_processor::PythonCallbackStageProcessor
//!
//! A callable cannot ride the processor config: `Config` is a blanket impl over
//! `Serialize + DeserializeOwned`
//! (`runtime/streamlib-engine/src/core/processors/traits/config.rs:47-51`), and a
//! `Py<PyAny>` is neither. So the config carries a string token and the callable
//! is parked here under that token before the graph starts.
//!
//! This indirection is a spike artifact, not a proposal. The real design would
//! hand the callable to the processor directly; #1702 explicitly defers that
//! ("real callback-injection design" is listed as a product-design item for the
//! pivot doc). Nothing here should be read as an API recommendation.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use pyo3::prelude::*;

/// Opaque key carried in the processor config, resolved to a callable at setup.
pub type PythonCallbackRegistrationToken = String;

fn python_callback_slots()
-> &'static Mutex<HashMap<PythonCallbackRegistrationToken, Py<PyAny>>> {
    static PYTHON_CALLBACK_SLOTS: OnceLock<
        Mutex<HashMap<PythonCallbackRegistrationToken, Py<PyAny>>>,
    > = OnceLock::new();
    PYTHON_CALLBACK_SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Park `callable` under `token`, replacing any callable already registered
/// there and returning it.
pub fn register_python_callback_under_token(
    token: PythonCallbackRegistrationToken,
    callable: Py<PyAny>,
) -> Option<Py<PyAny>> {
    python_callback_slots()
        .lock()
        .expect("python callback registry mutex is never held across a panic")
        .insert(token, callable)
}

/// Resolve `token` to a new reference to its callable, or `None` if nothing was
/// registered under it.
pub fn resolve_python_callback_for_token(
    python: Python<'_>,
    token: &str,
) -> Option<Py<PyAny>> {
    python_callback_slots()
        .lock()
        .expect("python callback registry mutex is never held across a panic")
        .get(token)
        .map(|callable| callable.clone_ref(python))
}

/// Drop the callable registered under `token`, returning it if there was one.
pub fn unregister_python_callback_for_token(token: &str) -> Option<Py<PyAny>> {
    python_callback_slots()
        .lock()
        .expect("python callback registry mutex is never held across a panic")
        .remove(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialize_interpreter_once() {
        static INTERPRETER_INITIALIZED: OnceLock<()> = OnceLock::new();
        INTERPRETER_INITIALIZED.get_or_init(Python::initialize);
    }

    /// The registry's whole purpose: a callable parked before start is
    /// retrievable from a different thread once the graph is running.
    #[test]
    fn a_registered_callable_resolves_from_a_foreign_thread() {
        initialize_interpreter_once();
        let token = "resolves-from-foreign-thread".to_string();
        Python::attach(|python| {
            let callable = python
                .eval(pyo3::ffi::c_str!("lambda value: value + 1"), None, None)
                .expect("lambda compiles")
                .unbind();
            register_python_callback_under_token(token.clone(), callable);
        });

        let token_for_thread = token.clone();
        let observed_result = std::thread::spawn(move || {
            Python::attach(|python| {
                let callable = resolve_python_callback_for_token(python, &token_for_thread)
                    .expect("the callable parked on the main thread resolves here");
                callable
                    .call1(python, (41i64,))
                    .expect("call succeeds")
                    .extract::<i64>(python)
                    .expect("returns an int")
            })
        })
        .join()
        .expect("callback thread does not panic");

        assert_eq!(observed_result, 42);
        unregister_python_callback_for_token(&token);
    }

    /// An unregistered token must resolve to `None` rather than panicking, so
    /// the stage can surface a typed configuration error instead of aborting a
    /// measurement run mid-cell.
    #[test]
    fn an_unknown_token_resolves_to_none() {
        initialize_interpreter_once();
        Python::attach(|python| {
            assert!(resolve_python_callback_for_token(python, "never-registered").is_none());
        });
    }

    /// Unregistering must actually release the slot — a leaked callable would
    /// keep an interpreter reference alive past teardown and confound the
    /// warm-restart battery's RSS readings.
    #[test]
    fn unregistering_releases_the_slot() {
        initialize_interpreter_once();
        let token = "released-on-unregister".to_string();
        Python::attach(|python| {
            let callable = python
                .eval(pyo3::ffi::c_str!("lambda value: value"), None, None)
                .expect("lambda compiles")
                .unbind();
            register_python_callback_under_token(token.clone(), callable);
        });
        assert!(unregister_python_callback_for_token(&token).is_some());
        Python::attach(|python| {
            assert!(resolve_python_callback_for_token(python, &token).is_none());
        });
    }
}
