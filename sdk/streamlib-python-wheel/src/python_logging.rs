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
use streamlib::sdk::iceoryx2::TheClockAnInboundLinksStampsAreTakenOn;
use streamlib::sdk::logging::{
    self as engine_logging, EngineLogRecordForTheParentProcess, HelperProcessEngineLogRecordRing,
    LogLevel, emit_app_process_python_log_record, log_dir,
};
use streamlib::sdk::media_clock::MediaClock;

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};

/// The ring this helper's captured engine records queue in, waiting for the
/// thread that forwards them. Set once: one process holds one `tracing`
/// subscriber, so a second capture has nothing to install.
static THE_RING_THIS_HELPERS_ENGINE_LOG_RECORDS_QUEUE_IN: OnceLock<
    HelperProcessEngineLogRecordRing,
> = OnceLock::new();

/// Start capturing this helper process's engine `tracing` records, iceoryx2's
/// own included, for [`drain_the_engine_log_records_this_helper_captured`] to
/// hand the parent.
///
/// Called by `streamlib._helper` once its channel to the parent is up and
/// before it opens anything, and by nothing else. Refuses a second call: the
/// records of a process that already installed a subscriber are already going
/// somewhere.
#[pyfunction]
pub(crate) fn capture_this_helper_processes_engine_log_records(python: Python<'_>) -> PyResult<()> {
    let ring = python
        .detach(engine_logging::capture_this_helper_processes_engine_log_records)
        .map_err(|capture_failure| PyRuntimeError::new_err(capture_failure.to_string()))?;
    THE_RING_THIS_HELPERS_ENGINE_LOG_RECORDS_QUEUE_IN
        .set(ring)
        .map_err(|_| {
            PyRuntimeError::new_err("this process is already capturing the engine's log records")
        })
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
    let ring = THE_RING_THIS_HELPERS_ENGINE_LOG_RECORDS_QUEUE_IN
        .get()
        .ok_or_else(|| {
            PyRuntimeError::new_err(
                "this process is not capturing the engine's log records, so there are none to \
                 drain",
            )
        })?;
    // A wait that is not a duration — infinite, or past what `Duration` holds
    // — waits no time at all rather than panicking through the binding.
    let wait_for_the_first_record =
        Duration::try_from_secs_f64(wait_seconds).unwrap_or(Duration::ZERO);
    let drained = python.detach(|| ring.drain_waiting_at_most(wait_for_the_first_record));
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

/// Current monotonic time in nanoseconds, on the engine's `MediaClock`.
///
/// `CLOCK_MONOTONIC` on Linux and `mach_absolute_time` on macOS, so a value is
/// comparable with every bag stamp the engine takes on this machine.
#[pyfunction]
pub(crate) fn monotonic_now_ns() -> u64 {
    monotonic_clock_now_ns()
}

/// Which machine's monotonic clock [`monotonic_now_ns`] reads, as that
/// machine's boot-session UUID text.
///
/// The same string `inbound_link_stamp_clock_identity` answers for a link, so
/// a processor holding one link's machine has something to compare it against:
/// a stamp may be aged against a reading taken here exactly when the two
/// strings match.
#[pyfunction]
pub(crate) fn this_machines_stamp_clock_identity() -> Option<String> {
    // Asked of the engine as a link from this runtime asks it, rather than
    // spelling the same derivation a third time: what a local link answers is
    // exactly what this has to agree with, so it comes from that arm itself.
    TheClockAnInboundLinksStampsAreTakenOn::ThisMachine
        .what_is_known_of_it()
        .the_machine_if_it_is_known()
        .map(|machine| machine.to_string())
}

/// The directory the engine writes its per-runtime JSONL logs into.
//
// `PathBuf`, not `String`: pyo3 encodes it with surrogateescape, so a path that
// is not valid UTF-8 round-trips back through `open()`.
#[pyfunction]
pub(crate) fn runtime_log_directory() -> std::path::PathBuf {
    log_dir()
}

/// [`MediaClock::now`] in nanoseconds, shared by the clock binding, the
/// default output stamp, `ctx.time`, and `MonotonicTimer`'s deadlines.
pub(crate) fn monotonic_clock_now_ns() -> u64 {
    MediaClock::now().as_nanos() as u64
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

    /// The kernel clock the plan names for this platform, read without going
    /// through [`MediaClock`] so the domain check cannot agree with itself.
    fn platform_media_clock_read_directly_ns() -> u64 {
        #[cfg(target_os = "macos")]
        let clock_id = libc::CLOCK_UPTIME_RAW;
        #[cfg(not(target_os = "macos"))]
        let clock_id = libc::CLOCK_MONOTONIC;
        let mut timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `timespec` is a valid stack slot and the clock exists on
        // the platform it is selected for.
        unsafe { libc::clock_gettime(clock_id, &mut timespec) };
        timespec.tv_sec as u64 * 1_000_000_000 + timespec.tv_nsec as u64
    }

    /// `CLOCK_MONOTONIC` on Linux; on macOS `CLOCK_UPTIME_RAW`, which is
    /// `mach_absolute_time` in nanoseconds and stops across sleep.
    #[test]
    fn monotonic_clock_reads_the_kernel_clock_the_plan_names_for_this_platform() {
        let before = platform_media_clock_read_directly_ns();
        let binding = monotonic_clock_now_ns();
        let after = platform_media_clock_read_directly_ns();
        // `mach_absolute_time` ticks are coarser than a nanosecond, and the
        // kernel and `MediaClock` round a tick to nanoseconds separately.
        const TICK_ROUNDING_SLACK_NS: u64 = 1_000;
        assert!(
            before.saturating_sub(TICK_ROUNDING_SLACK_NS) <= binding
                && binding <= after + TICK_ROUNDING_SLACK_NS,
            "the wheel's clock ({binding}) fell outside the kernel bracket [{before}, {after}]"
        );
    }

    /// A wheel stamp and an engine stamp taken back to back are one clock.
    #[test]
    fn a_wheel_stamp_and_an_engine_stamp_taken_back_to_back_differ_by_microseconds() {
        let engine_before = MediaClock::now().as_nanos() as u64;
        let wheel = monotonic_clock_now_ns();
        let engine_after = MediaClock::now().as_nanos() as u64;
        assert!(
            engine_before <= wheel && wheel <= engine_after,
            "wheel stamp {wheel} fell outside engine bracket [{engine_before}, {engine_after}]"
        );
        assert!(
            engine_after - engine_before < 1_000_000,
            "back-to-back reads spanned {}ns",
            engine_after - engine_before
        );
    }
}
