// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Python object a node's scope holds, compared with `==` and closed when the scope lets go of
//! a value one of its builds made.

use std::collections::HashMap;
use std::sync::LazyLock;

use pyo3::prelude::*;

use crate::python_attached_from_any_thread::attached_from_any_thread;

/// What holds one Python object in the scopes of this process.
struct HoldersOfAPythonObject {
    holders: usize,
    /// Whether the scope's first hold of it was a build's value, rather than an input's: only an
    /// object a build made is the scope's to close.
    made_by_a_build: bool,
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
/// equal when they are one object or `==` says so; an `==` that raises counts as a change. An
/// object a build made is closed when the last holder of it lets it go.
pub(super) struct PythonValueInANodeScope {
    object: Option<Py<PyAny>>,
}

impl PythonValueInANodeScope {
    fn held_as(object: Py<PyAny>, held_as: HeldAs) -> Self {
        HOLDERS_OF_EACH_PYTHON_OBJECT
            .lock()
            .entry(object.as_ptr() as usize)
            .or_insert(HoldersOfAPythonObject {
                holders: 0,
                made_by_a_build: held_as == HeldAs::AComputedValue,
            })
            .holders += 1;
        Self {
            object: Some(object),
        }
    }

    pub(super) fn an_input_value(object: Py<PyAny>) -> Self {
        Self::held_as(object, HeldAs::AnInputValue)
    }

    pub(super) fn a_computed_value(object: Py<PyAny>) -> Self {
        Self::held_as(object, HeldAs::AComputedValue)
    }

    pub(super) fn object(&self) -> &Py<PyAny> {
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
        attached_from_any_thread(|python| {
            let object = object.into_bound(python);
            if is_closed_as_its_last_holder_lets_go(&object) {
                close_the_computed_value(python, &object);
            }
        });
    }
}

fn is_closed_as_its_last_holder_lets_go(object: &Bound<'_, PyAny>) -> bool {
    let mut holders = HOLDERS_OF_EACH_PYTHON_OBJECT.lock();
    let address = object.as_ptr() as usize;
    let Some(holders_of_the_object) = holders.get_mut(&address) else {
        return false;
    };
    holders_of_the_object.holders -= 1;
    if holders_of_the_object.holders > 0 {
        return false;
    }
    holders
        .remove(&address)
        .is_some_and(|holders_of_the_object| holders_of_the_object.made_by_a_build)
}

fn close_the_computed_value(python: Python<'_>, value: &Bound<'_, PyAny>) {
    let Ok(close) = value.getattr("close") else {
        return;
    };
    if let Err(raised) = close.call0() {
        raised.write_unraisable(python, Some(value));
    }
}
