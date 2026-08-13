// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Logging and timekeeping for the app's own Python code.
//!
//! In the app process, a log line goes straight into the engine's unified
//! JSONL pipeline — the same drain the engine's own records go through, so
//! the app's output interleaves with the engine's in one ordered stream
//! instead of arriving as captured stdout. A processor's records take the
//! other route: its helper process forwards them over the escalate `Log` op,
//! and the parent stamps and enqueues them into this same pipeline.

use std::collections::BTreeMap;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib::sdk::logging::{LogLevel, emit_app_process_python_log_record, log_dir};

use crate::python_bag_conversion::python_object_to_json_value;

/// Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`.
///
/// The kernel's `CLOCK_MONOTONIC` epoch, so values are comparable across
/// processes — the same domain Python's
/// `time.clock_gettime_ns(time.CLOCK_MONOTONIC)` reads. Matches the engine's
/// bag stamps on Linux; on Apple the engine stamps with `mach_absolute_time`,
/// which stops across system sleep.
#[pyfunction]
pub(crate) fn monotonic_now_ns() -> u64 {
    monotonic_clock_now_ns()
}

/// The directory the engine writes its per-runtime JSONL logs into.
//
// `PathBuf`, not `String`: pyo3 encodes it with surrogateescape, so a path that
// is not valid UTF-8 round-trips back through `open()`.
#[pyfunction]
pub(crate) fn runtime_log_directory() -> std::path::PathBuf {
    log_dir()
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

/// Emit one record on the engine's log pipeline, with structured attrs.
///
/// This is the app process's own Python logging. A processor's records never
/// come through here — it runs in its own child, whose `streamlib.log` routes
/// to the parent over the escalate `Log` op.
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
    emit_app_process_python_log_record(level, message.to_string(), attribute_map);
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
    /// `time.clock_gettime_ns(time.CLOCK_MONOTONIC)` reads.
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
