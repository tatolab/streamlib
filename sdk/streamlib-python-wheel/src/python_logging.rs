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
//!
//! The engine's own records made inside a helper take that same route: the
//! helper installs the capture below, and its forwarding thread drains what
//! the engine wrote into the ring and sends each record on as the parent's
//! `source: "rust"`.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use streamlib::sdk::logging::{
    EngineLogRecordForTheParentProcess, HelperProcessEngineLogRecordRing, LogLevel,
    capture_this_helper_processes_engine_log_records, emit_app_process_python_log_record, log_dir,
};

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};

/// This process's captured engine records, waiting for the thread that
/// forwards them. Set once: one process holds one `tracing` subscriber, so a
/// second capture has nothing to install.
static ENGINE_LOG_RECORDS_THIS_HELPER_CAPTURED: OnceLock<HelperProcessEngineLogRecordRing> =
    OnceLock::new();

/// Start capturing this helper process's engine `tracing` records, iceoryx2's
/// own included, for [`drain_the_engine_log_records_this_helper_captured`] to
/// hand the parent.
///
/// Called by `streamlib._helper` once its channel to the parent is up and
/// before it opens anything, and by nothing else. Refuses a second call: the
/// records of a process that already installed a subscriber are already going
/// somewhere.
#[pyfunction]
pub(crate) fn capture_this_processes_engine_log_records(python: Python<'_>) -> PyResult<()> {
    if ENGINE_LOG_RECORDS_THIS_HELPER_CAPTURED.get().is_some() {
        return Err(PyRuntimeError::new_err(
            "this process is already capturing the engine's log records",
        ));
    }
    let ring = python
        .detach(capture_this_helper_processes_engine_log_records)
        .map_err(|capture_failure| PyRuntimeError::new_err(capture_failure.to_string()))?;
    let _ = ENGINE_LOG_RECORDS_THIS_HELPER_CAPTURED.set(ring);
    Ok(())
}

/// Take every engine record captured so far, waiting up to `wait_seconds` for
/// the first one, and say how many the ring dropped since the last drain.
///
/// Answers `(records, dropped_record_count)`, each record a mapping of the
/// JSONL columns it fills. The wait happens with the GIL released, so the
/// forwarding thread parks here instead of holding up the processor's own.
#[pyfunction]
pub(crate) fn drain_the_engine_log_records_this_helper_captured(
    python: Python<'_>,
    wait_seconds: f64,
) -> PyResult<(Py<PyList>, u64)> {
    let ring = ENGINE_LOG_RECORDS_THIS_HELPER_CAPTURED.get().ok_or_else(|| {
        PyRuntimeError::new_err(
            "this process is not capturing the engine's log records, so there are none to drain",
        )
    })?;
    let drained = python
        .detach(|| ring.drain_waiting_at_most(Duration::from_secs_f64(wait_seconds.max(0.0))));
    let records = PyList::empty(python);
    for record in drained.records {
        records.append(engine_log_record_as_python_mapping(python, record)?)?;
    }
    Ok((
        records.unbind(),
        drained.records_dropped_since_the_last_drain,
    ))
}

/// One captured record as the mapping the forwarding thread reads its columns
/// off.
fn engine_log_record_as_python_mapping<'py>(
    python: Python<'py>,
    record: EngineLogRecordForTheParentProcess,
) -> PyResult<Bound<'py, PyDict>> {
    let attrs = PyDict::new(python);
    for (key, value) in record.attrs.iter() {
        attrs.set_item(key, json_value_to_python_object(python, value)?)?;
    }
    let mapping = PyDict::new(python);
    mapping.set_item("level", record.level.as_str())?;
    mapping.set_item("target", record.target)?;
    mapping.set_item("message", record.message)?;
    mapping.set_item("pipeline_id", record.pipeline_id)?;
    mapping.set_item("processor_id", record.processor_id)?;
    mapping.set_item("rhi_op", record.rhi_op)?;
    mapping.set_item("attrs", attrs)?;
    mapping.set_item(
        "emitted_at_wall_clock_nanoseconds",
        record.emitted_at_wall_clock_nanoseconds,
    )?;
    Ok(mapping)
}

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
