// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Logging and timekeeping for Python processors.
//!
//! In-process, a log line is a direct call into `tracing` — the same pipeline
//! the engine's own records go through, so a processor's output interleaves
//! with the engine's in one ordered stream instead of arriving as captured
//! stdout.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::media_clock::MediaClock;

/// The `tracing` target every record from Python carries.
///
/// Underscored rather than `streamlib::python`: a `streamlib::` literal reads
/// to `cargo xtask check-boundaries` as a forbidden top-level path shortcut.
const PYTHON_LOG_TARGET: &str = "streamlib_python";

/// The clock the engine stamps bags with, in nanoseconds.
///
/// Monotonic and shared by every processor in this process, so two readings
/// subtract to a real duration. It is not the system-wide `CLOCK_MONOTONIC`
/// epoch — the origin is this process's engine start — so a value from one
/// process means nothing in another.
#[pyfunction]
pub(crate) fn media_clock_now_ns() -> u64 {
    MediaClock::now().as_nanos() as u64
}

/// Emit one record on the engine's log pipeline.
///
/// The level is resolved here rather than exposed as five bindings because
/// `tracing`'s macros need a compile-time level; `emitted_by` names the
/// processor so records stay attributable in a graph of many.
#[pyfunction]
pub(crate) fn log_event(level: &str, emitted_by: &str, message: &str) -> PyResult<()> {
    match level {
        "trace" => tracing::trace!(target: PYTHON_LOG_TARGET, %emitted_by, "{message}"),
        "debug" => tracing::debug!(target: PYTHON_LOG_TARGET, %emitted_by, "{message}"),
        "info" => tracing::info!(target: PYTHON_LOG_TARGET, %emitted_by, "{message}"),
        "warn" => tracing::warn!(target: PYTHON_LOG_TARGET, %emitted_by, "{message}"),
        "error" => tracing::error!(target: PYTHON_LOG_TARGET, %emitted_by, "{message}"),
        unknown => {
            return Err(PyValueError::new_err(format!(
                "unknown log level {unknown:?}: expected trace, debug, info, warn or error"
            )));
        }
    }
    Ok(())
}
