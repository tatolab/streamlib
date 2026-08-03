// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of the streamlib wheel — the extension module CPython
//! imports as `streamlib._engine`.

use pyo3::prelude::*;

mod python_runtime_lifecycle;

pub use python_runtime_lifecycle::PythonRuntimeHandle;

#[pymodule]
fn _engine(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonRuntimeHandle>()?;
    Ok(())
}
