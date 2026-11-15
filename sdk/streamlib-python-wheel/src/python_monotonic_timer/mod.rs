// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Drift-free periodic timer for Python processors.
//!
//! The first deadline is now + interval and every repeat is absolute on the
//! engine's `MediaClock`, so latency in one tick never accumulates into the
//! next. Linux arms a `timerfd` with `TFD_TIMER_ABSTIME` and waits through an
//! epoll fd; macOS arms a one-shot kqueue `EVFILT_TIMER` at each next deadline
//! in `mach_absolute_time` ticks. Either wait honors a timeout, which is what
//! bounds teardown latency for a loop polling shutdown between ticks.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod monotonic_timer_absolute_deadline_grid;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod neither_linux_nor_macos;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

use parking_lot::Mutex;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

#[cfg(target_os = "linux")]
use linux::{
    MonotonicTimerFileDescriptors, close_python_monotonic_timer, open_python_monotonic_timer,
    wait_on_python_monotonic_timer,
};
#[cfg(target_os = "macos")]
use macos::{
    MonotonicTimerKqueueSchedule, close_python_monotonic_timer, open_python_monotonic_timer,
    wait_on_python_monotonic_timer,
};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use neither_linux_nor_macos::{
    close_python_monotonic_timer, open_python_monotonic_timer, wait_on_python_monotonic_timer,
};

/// Periodic monotonic timer, used as `with start_monotonic_timer(interval_ns) as timer:`.
#[pyclass(name = "MonotonicTimer", module = "tatolab.stream", frozen)]
pub(crate) struct PythonMonotonicTimer {
    timer_interval_ns: i64,
    #[cfg(target_os = "linux")]
    file_descriptors: Mutex<Option<MonotonicTimerFileDescriptors>>,
    #[cfg(target_os = "macos")]
    kqueue_schedule: Mutex<Option<MonotonicTimerKqueueSchedule>>,
}

#[pymethods]
impl PythonMonotonicTimer {
    #[new]
    fn new(interval_ns: i64) -> PyResult<Self> {
        if interval_ns <= 0 {
            return Err(PyValueError::new_err(format!(
                "interval_ns must be > 0, got {interval_ns}"
            )));
        }
        open_python_monotonic_timer(interval_ns)
    }

    #[getter]
    fn interval_ns(&self) -> i64 {
        self.timer_interval_ns
    }

    /// Wait up to `timeout_ms` for the next tick.
    ///
    /// Returns the positive expiration count when a tick fired, `0` on
    /// timeout (poll shutdown and call again), `-1` after `close()` or on
    /// error.
    #[pyo3(signature = (timeout_ms = 100))]
    fn wait(&self, python: Python<'_>, timeout_ms: u64) -> i64 {
        wait_on_python_monotonic_timer(self, python, timeout_ms)
    }

    /// Close the timer. Idempotent; a subsequent `wait` returns `-1`.
    fn close(&self) {
        close_python_monotonic_timer(self);
    }

    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self
    }

    #[pyo3(signature = (exception_type = None, exception = None, traceback = None))]
    fn __exit__(
        &self,
        exception_type: Option<&Bound<'_, PyAny>>,
        exception: Option<&Bound<'_, PyAny>>,
        traceback: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        let _ = (exception_type, exception, traceback);
        self.close();
        false
    }
}

impl Drop for PythonMonotonicTimer {
    fn drop(&mut self) {
        self.close();
    }
}
