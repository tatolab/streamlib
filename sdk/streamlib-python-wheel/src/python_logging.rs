// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Logging and timekeeping for Python processors.
//!
//! In-process, a log line goes straight into the engine's unified JSONL
//! pipeline — the same drain the engine's own records go through, so a
//! processor's output interleaves with the engine's in one ordered stream
//! instead of arriving as captured stdout.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib::sdk::logging::{LogLevel, emit_python_processor_log_record};
use streamlib::sdk::media_clock::MediaClock;

use crate::python_bag_conversion::python_object_to_json_value;

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

/// Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`.
///
/// The kernel's monotonic epoch, so values are comparable across processes —
/// to Python's `time.clock_gettime_ns(time.CLOCK_MONOTONIC)`, to Rust
/// `Instant` reads, and to the old SDK's canonical bag stamps. This is a
/// different epoch from [`media_clock_now_ns`]; both are documented, neither
/// is derived from the other.
#[pyfunction]
pub(crate) fn monotonic_now_ns() -> u64 {
    monotonic_clock_now_ns()
}

/// Raw `CLOCK_MONOTONIC` in nanoseconds, shared by the clock binding, the
/// default output stamp, and `ctx.time`.
pub(crate) fn monotonic_clock_now_ns() -> u64 {
    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timespec` is a valid stack slot; CLOCK_MONOTONIC exists on
    // every platform the wheel targets, so the call cannot fail with these
    // arguments.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
    (timespec.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(timespec.tv_nsec as u64)
}

/// Which processor's hook is running on this thread, for log attribution.
///
/// Set by the host around every lifecycle hook; a `log_event` outside any
/// hook (module import time, a worker thread) carries no attribution.
pub(crate) struct PythonProcessorLogAttribution {
    pub(crate) processor_id: Option<String>,
    pub(crate) processor_display_name: String,
}

thread_local! {
    static CURRENT_PYTHON_PROCESSOR_LOG_ATTRIBUTION:
        RefCell<Option<Arc<PythonProcessorLogAttribution>>> = const { RefCell::new(None) };
}

pub(crate) fn set_current_python_processor_log_attribution(
    attribution: Option<Arc<PythonProcessorLogAttribution>>,
) {
    CURRENT_PYTHON_PROCESSOR_LOG_ATTRIBUTION.with(|current| *current.borrow_mut() = attribution);
}

fn current_python_processor_log_attribution() -> Option<Arc<PythonProcessorLogAttribution>> {
    CURRENT_PYTHON_PROCESSOR_LOG_ATTRIBUTION.with(|current| current.borrow().clone())
}

/// Emit one record on the engine's log pipeline, with structured attrs.
///
/// The processor attribution comes from the host's per-thread marker rather
/// than a parameter, so a helper deep inside user code attributes correctly
/// without threading a context through.
#[pyfunction]
#[pyo3(signature = (level, message, attrs = None))]
pub(crate) fn log_event(
    level: &str,
    message: &str,
    attrs: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    let level = parse_log_level_name(level)?;
    let mut attribute_map = BTreeMap::new();
    if let Some(attrs) = attrs {
        for (key, value) in attrs.iter() {
            let key = key.extract::<String>().map_err(|_| {
                PyValueError::new_err("log attr keys must be strings — they become JSONL columns")
            })?;
            attribute_map.insert(key, python_object_to_json_value(&value)?);
        }
    }
    let processor_id = current_python_processor_log_attribution().map(|attribution| {
        attribution
            .processor_id
            .clone()
            .unwrap_or_else(|| attribution.processor_display_name.clone())
    });
    emit_python_processor_log_record(level, message.to_string(), processor_id, attribute_map);
    Ok(())
}

fn parse_log_level_name(level: &str) -> PyResult<LogLevel> {
    match level {
        "trace" => Ok(LogLevel::Trace),
        "debug" => Ok(LogLevel::Debug),
        "info" => Ok(LogLevel::Info),
        "warn" => Ok(LogLevel::Warn),
        "error" => Ok(LogLevel::Error),
        unknown => Err(PyValueError::new_err(format!(
            "unknown log level {unknown:?}: expected trace, debug, info, warn or error"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two reads never go backwards — the monotonic contract.
    #[test]
    fn monotonic_clock_never_goes_backwards() {
        let first = monotonic_clock_now_ns();
        let second = monotonic_clock_now_ns();
        assert!(second >= first, "clock went backwards: {first} -> {second}");
    }

    /// The value domain is the kernel's CLOCK_MONOTONIC epoch — the same one
    /// `time.clock_gettime_ns(time.CLOCK_MONOTONIC)` reads — not a
    /// process-local origin like the media clock's.
    #[test]
    fn monotonic_clock_shares_the_kernel_clock_monotonic_domain() {
        let mut timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
        let direct = (timespec.tv_sec as u64) * 1_000_000_000 + timespec.tv_nsec as u64;
        let binding = monotonic_clock_now_ns();
        assert!(
            binding.abs_diff(direct) < 1_000_000_000,
            "readings a moment apart landed in different domains: {direct} vs {binding}"
        );
    }
}
