// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSONL event schema — the durable interface contract for the unified
//! logging pathway. Every record written to `$XDG_STATE_HOME/streamlib/logs/`
//! is one [`RuntimeLogEvent`] per line.
//!
//! Adding fields is backwards-compatible. Renaming, removing, or changing
//! types of existing fields requires bumping [`SCHEMA_VERSION`] and a
//! coordinated update across every downstream consumer (CLI, orchestrator,
//! polyglot SDKs).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Top-level schema version. Bumped on any breaking change.
pub const SCHEMA_VERSION: u32 = 1;

/// Origin of a log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Rust,
    Python,
    Deno,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Rust => "rust",
            Source::Python => "python",
            Source::Deno => "deno",
        }
    }
}

/// Severity level of a log record. Mirrors `tracing::Level` ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

impl From<tracing::Level> for LogLevel {
    fn from(level: tracing::Level) -> Self {
        match level {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        }
    }
}

/// One JSONL line. Every field is emitted on every line; `null` where not
/// applicable.
///
/// Field nullability and semantics are the load-bearing contract — see
/// `docs/logging-schema.md`. Downstream children of #430 (`streamlib-cli
/// logs`, polyglot SDKs, the future orchestrator) depend on this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeLogEvent {
    /// Schema version of this record. Bumped on breaking changes.
    pub schema_version: u32,

    /// Host monotonic receipt timestamp, nanoseconds since UNIX epoch.
    /// Authoritative sort key across the merged stream.
    pub host_ts: u64,

    /// Unique runtime identifier (from [`RuntimeUniqueId`]).
    pub runtime_id: String,

    /// Language / origin of the record.
    pub source: Source,

    /// Severity.
    pub level: LogLevel,

    /// Primary human-readable message. Corresponds to the `message` field
    /// of a `tracing::*!()` call, or the first positional argument of a
    /// polyglot `streamlib.log.*` call.
    pub message: String,

    /// `tracing` target (module path, typically).
    pub target: String,

    /// Pipeline identifier. `None` for runtime-level events.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pipeline_id: Option<String>,

    /// Processor identifier. `None` for events outside a processor.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub processor_id: Option<String>,

    /// RHI operation name (e.g. `acquire_texture`, `acquire_pixel_buffer`).
    /// Set only inside RHI call sites.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rhi_op: Option<String>,

    /// Subprocess wall-clock timestamp ISO8601 (advisory; never used for
    /// ordering). Set only when `source != Rust`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_ts: Option<String>,

    /// Subprocess-monotonic sequence number. Escape hatch for recovering
    /// subprocess-local order. Set only when `source != Rust`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_seq: Option<u64>,

    /// `true` when the record came from an interceptor (captured `print()`,
    /// `console.log`, raw fd write, etc.) rather than a direct tracing call.
    #[serde(default)]
    pub intercepted: bool,

    /// Interceptor channel identifier when `intercepted: true`.
    /// E.g. `"stdout"`, `"stderr"`, `"console.log"`, `"logging"`, `"fd1"`,
    /// `"fd2"`. `None` when `intercepted: false`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub channel: Option<String>,

    /// User-supplied structured fields captured from the emitting call
    /// site. Values are whatever `tracing`'s `Visit` trait captured
    /// (formatted via `Display`/`Debug`), or the polyglot-supplied
    /// `attrs` map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_round_trips() {
        let ev = RuntimeLogEvent {
            schema_version: SCHEMA_VERSION,
            host_ts: 1_700_000_000_000_000_000,
            runtime_id: "Rabc".into(),
            source: Source::Rust,
            level: LogLevel::Info,
            message: "hi".into(),
            target: "streamlib::core::logging::tests".into(),
            pipeline_id: None,
            processor_id: None,
            rhi_op: None,
            source_ts: None,
            source_seq: None,
            intercepted: false,
            channel: None,
            attrs: BTreeMap::new(),
        };
        let line = serde_json::to_string(&ev).unwrap();
        let back: RuntimeLogEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.runtime_id, "Rabc");
        assert_eq!(back.source, Source::Rust);
        assert_eq!(back.message, "hi");
    }

    #[test]
    fn source_and_level_serialize_lowercase() {
        assert_eq!(serde_json::to_string(&Source::Rust).unwrap(), "\"rust\"");
        assert_eq!(serde_json::to_string(&Source::Python).unwrap(), "\"python\"");
        assert_eq!(serde_json::to_string(&Source::Deno).unwrap(), "\"deno\"");
        assert_eq!(serde_json::to_string(&LogLevel::Warn).unwrap(), "\"warn\"");
    }

    /// Parses the exact example line documented in `docs/logging-schema.md`.
    /// If this test fails, the published schema example and the
    /// implementation have drifted — fix one or the other.
    #[test]
    fn docs_example_line_parses() {
        let line = r#"{"schema_version":1,"host_ts":1700000000000000000,"runtime_id":"Rabc123","source":"rust","level":"info","message":"processor started","target":"streamlib::linux::processors::camera","pipeline_id":"pl-42","processor_id":"camera-1","rhi_op":null,"intercepted":false,"attrs":{"device":"/dev/video0"}}"#;
        let ev: RuntimeLogEvent = serde_json::from_str(line).expect("docs example must parse");
        assert_eq!(ev.schema_version, SCHEMA_VERSION);
        assert_eq!(ev.runtime_id, "Rabc123");
        assert_eq!(ev.source, Source::Rust);
        assert_eq!(ev.level, LogLevel::Info);
        assert_eq!(ev.pipeline_id.as_deref(), Some("pl-42"));
        assert_eq!(ev.processor_id.as_deref(), Some("camera-1"));
        assert!(ev.rhi_op.is_none());
        assert!(!ev.intercepted);
        assert_eq!(
            ev.attrs.get("device"),
            Some(&serde_json::Value::String("/dev/video0".into()))
        );
    }
}
