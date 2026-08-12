// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Building a class the way a user's module would, for tests that read
//! attributes CPython assigns rather than attributes a test set by hand.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Run `source` as a module body and hand back the class it bound to
/// `class_name`.
///
/// Running Python is the point: `__module__` and `__qualname__` are what
/// CPython actually assigns, so a test asserting on them is not asserting on
/// its own setup.
pub(crate) fn class_from_source<'py>(
    python: Python<'py>,
    source: &str,
    class_name: &str,
) -> Bound<'py, PyAny> {
    let namespace = PyDict::new(python);
    python
        .run(
            &std::ffi::CString::new(source).unwrap(),
            Some(&namespace),
            None,
        )
        .unwrap();
    namespace.get_item(class_name).unwrap().unwrap()
}
