// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use super::PythonMonotonicTimer;

pub(super) fn open_python_monotonic_timer(interval_ns: i64) -> PyResult<PythonMonotonicTimer> {
    let _ = interval_ns;
    Err(PyRuntimeError::new_err(
        "MonotonicTimer has no timer backend on this platform",
    ))
}

pub(super) fn wait_on_python_monotonic_timer(
    timer: &PythonMonotonicTimer,
    python: Python<'_>,
    timeout_ms: u64,
) -> i64 {
    let _ = (timer, python, timeout_ms);
    -1
}

pub(super) fn close_python_monotonic_timer(timer: &PythonMonotonicTimer) {
    let _ = timer;
}
