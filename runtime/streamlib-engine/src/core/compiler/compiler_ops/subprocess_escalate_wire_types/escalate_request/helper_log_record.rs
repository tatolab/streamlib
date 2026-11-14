// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Severity level of the record. Maps 1:1 onto tracing::Level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestLogLevel {
    #[serde(rename = "debug")]
    Debug,

    #[serde(rename = "error")]
    Error,

    #[serde(rename = "info")]
    Info,

    #[serde(rename = "trace")]
    Trace,

    #[serde(rename = "warn")]
    Warn,
}

/// Origin runtime of the record: "python" for a `tatolab.stream.log` call in the
/// helper, "rust" for an engine `tracing` record the helper captured and
/// drained. An engine record made in the app process never comes this way —
/// it reaches the pipeline through `tracing::*!()` directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestLogSource {
    #[serde(rename = "python")]
    Python,

    #[serde(rename = "rust")]
    Rust,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestLog {
    /// User-supplied structured fields. Copied flat onto the emitted
    /// RuntimeLogEvent's `attrs` map — not nested under an `attrs.key` path in
    /// the JSONL.
    pub(crate) attrs: HashMap<String, Option<Value>>,

    /// Interceptor channel when `intercepted: true`. Conventional values:
    /// "stdout", "stderr", "console.log", "logging", "fd1", "fd2". Null when
    /// `intercepted: false`.
    pub(crate) channel: Option<String>,

    /// True when the record was captured from subprocess stdout/stderr,
    /// console.log, root logging handler, or a raw fd write, rather than a
    /// direct `tatolab.stream.log.*` call.
    pub(crate) intercepted: bool,

    /// Severity level of the record. Maps 1:1 onto tracing::Level.
    pub(crate) level: EscalateRequestLogLevel,

    /// Primary human-readable message.
    pub(crate) message: String,

    /// Pipeline identifier. Null for runtime-level records.
    pub(crate) pipeline_id: Option<String>,

    /// Processor identifier. Null outside a processor.
    pub(crate) processor_id: Option<String>,

    /// RHI operation the record was made inside, for a captured engine
    /// record that names one. Absent from a record that names none, and from
    /// every `tatolab.stream.log` call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rhi_op: Option<String>,

    /// Origin runtime of the record: "python" for a `tatolab.stream.log` call,
    /// "rust" for an engine `tracing` record the helper captured.
    pub(crate) source: EscalateRequestLogSource,

    /// Subprocess-monotonic sequence number (u64 as string — JSON has no
    /// native u64). Escape hatch for recovering subprocess-local order within
    /// a single source. Not authoritative across sources — use `host_ts` for
    /// merged-stream ordering.
    pub(crate) source_seq: String,

    /// Subprocess wall-clock timestamp ISO8601 (advisory). Never used for
    /// ordering; the host stamps `host_ts` on receipt as the authoritative
    /// sort key.
    pub(crate) source_ts: String,

    /// The record's `tracing` target, for a captured engine record. Absent
    /// from a `tatolab.stream.log` call, whose target is the source's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
}
