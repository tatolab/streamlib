// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A helper process's `log` op, turned into a host-side [`LogRecord`].

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestLog, EscalateRequestLogLevel, EscalateRequestLogSource,
};
use crate::core::logging::{LogLevel, LogRecord, Source};

/// Convert a wire-format [`EscalateRequestLog`] into a host-side
/// [`LogRecord`]. Stamps `host_ts` at the moment of receipt — the
/// subprocess-supplied `source_ts` is advisory only and never used for
/// ordering. Parses `source_seq` from its string wire encoding (JSON has
/// no 64-bit integer); silently drops the value on parse failure so a
/// malformed subprocess can't block log delivery.
pub(super) fn log_record_from_wire(log: EscalateRequestLog) -> LogRecord {
    let source = match log.source {
        EscalateRequestLogSource::Python => Source::Python,
        EscalateRequestLogSource::Rust => Source::Rust,
    };
    let level = match log.level {
        EscalateRequestLogLevel::Trace => LogLevel::Trace,
        EscalateRequestLogLevel::Debug => LogLevel::Debug,
        EscalateRequestLogLevel::Info => LogLevel::Info,
        EscalateRequestLogLevel::Warn => LogLevel::Warn,
        EscalateRequestLogLevel::Error => LogLevel::Error,
    };
    // A captured engine record carries the target of the call site that made
    // it, so it reads in the log exactly as it would from the app process; a
    // `streamlib.log` call has no target of its own and takes its source's.
    let target = log.target.unwrap_or_else(|| {
        match source {
            Source::Python => "streamlib::polyglot::python",
            Source::Rust => "streamlib::polyglot",
        }
        .to_string()
    });
    let source_seq = log.source_seq.parse::<u64>().ok();
    let attrs: BTreeMap<String, serde_json::Value> = log
        .attrs
        .into_iter()
        .map(|(k, v)| (k, v.unwrap_or(serde_json::Value::Null)))
        .collect();

    LogRecord {
        host_ts: now_ns(),
        level,
        target,
        message: log.message,
        pipeline_id: log.pipeline_id,
        processor_id: log.processor_id,
        rhi_op: log.rhi_op,
        intercepted: log.intercepted,
        channel: log.channel,
        attrs,
        source: Some(source),
        source_ts: Some(log.source_ts),
        source_seq,
    }
}

pub(super) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
