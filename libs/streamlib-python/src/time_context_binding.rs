// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for TimeContext.

use pyo3::prelude::*;
use std::sync::Arc;
use streamlib::TimeContext;

/// Python-accessible TimeContext for timing operations.
///
/// Provides a shared monotonic clock that starts when the runtime starts.
/// All processors share this clock for coordinated animations and timing.
///
/// Access via `ctx.time` in processor methods.
#[pyclass(name = "TimeContext")]
#[derive(Clone)]
pub struct PyTimeContext {
    inner: Arc<TimeContext>,
}

impl PyTimeContext {
    pub fn new(ctx: Arc<TimeContext>) -> Self {
        Self { inner: ctx }
    }
}

#[pymethods]
impl PyTimeContext {
    /// Seconds since the runtime started.
    ///
    /// Use this for animations and timing that needs to be coordinated
    /// across multiple processors.
    ///
    /// Example:
    ///     phase = ctx.time.elapsed_secs * 2 * math.pi  # 1Hz oscillation
    ///     amplitude = math.sin(phase)
    #[getter]
    fn elapsed_secs(&self) -> f64 {
        self.inner.elapsed_secs()
    }

    /// Nanoseconds since the runtime started.
    ///
    /// Higher precision than elapsed_secs for fine-grained timing.
    #[getter]
    fn elapsed_ns(&self) -> i64 {
        self.inner.elapsed_ns()
    }

    /// Raw monotonic clock value in nanoseconds.
    ///
    /// Useful for computing your own deltas or absolute timestamps.
    #[getter]
    fn now_ns(&self) -> i64 {
        self.inner.now_ns()
    }

    fn __repr__(&self) -> String {
        format!("TimeContext(elapsed={:.3}s)", self.inner.elapsed_secs())
    }
}
