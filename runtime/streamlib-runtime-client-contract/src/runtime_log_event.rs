// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSONL event schema — the durable interface contract for the unified
//! logging pathway. Every record written under
//! [`log_dir`](crate::runtime_log_file_paths::log_dir) is one [`RuntimeLogEvent`]
//! per line.
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
}

impl Source {
    /// Every origin a record can carry.
    pub const ALL: [Source; 2] = [Source::Rust, Source::Python];

    /// The lowercase name the JSONL record carries.
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Rust => "rust",
            Source::Python => "python",
        }
    }
}

/// Severity level of a log record, ordered from trace, the least severe, to error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// Every level, least severe first.
    pub const ALL: [LogLevel; 5] = [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ];

    /// The lowercase name the JSONL record carries.
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
/// `docs/logging-schema.md`. Every reader, the native `tatolab logs` among
/// them, depends on this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeLogEvent {
    /// Schema version of this record. Bumped on breaking changes.
    pub schema_version: u32,

    /// Host receipt wall-clock timestamp, nanoseconds since the UNIX epoch.
    /// Authoritative sort key across the merged stream.
    ///
    /// Wall clock, not monotonic: a log record's job is correlating StreamLib
    /// with the outside world and with other hosts' logs, which monotonic time
    /// cannot do. Never compare or subtract this against a media timestamp —
    /// they share a unit and are different quantities.
    pub host_ts: u64,

    /// The `RuntimeUniqueId` of the runtime whose stream emitted the record, verbatim; empty
    /// for a record no stream emitted, which reaches only the pretty mirror.
    pub runtime_id: String,

    /// The URL-safe cast name of the loaded stream that emitted the record. `None` for a
    /// record no stream emitted.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stream: Option<String>,

    /// Language / origin of the record.
    pub source: Source,

    /// Severity.
    pub level: LogLevel,

    /// Primary human-readable message. Corresponds to the `message` field
    /// of a `tracing::*!()` call, or the first positional argument of a
    /// polyglot `tatolab.stream.log.*` call.
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
            stream: Some("main".into()),
            source: Source::Rust,
            level: LogLevel::Info,
            message: "hi".into(),
            target: "streamlib::sdk::logging::tests".into(),
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
        assert_eq!(back.stream.as_deref(), Some("main"));
        assert_eq!(back.source, Source::Rust);
        assert_eq!(back.message, "hi");
    }

    #[test]
    fn source_and_level_serialize_lowercase() {
        assert_eq!(serde_json::to_string(&Source::Rust).unwrap(), "\"rust\"");
        assert_eq!(
            serde_json::to_string(&Source::Python).unwrap(),
            "\"python\""
        );
        assert_eq!(serde_json::to_string(&LogLevel::Warn).unwrap(), "\"warn\"");
    }

    /// A minimum-level floor admits a level by comparing it, so the order is severity's.
    #[test]
    fn levels_order_trace_below_debug_below_info_below_warn_below_error() {
        assert!(
            LogLevel::ALL.is_sorted_by(|lower, higher| lower < higher),
            "{:?}",
            LogLevel::ALL
        );
        assert_eq!(LogLevel::ALL.first(), Some(&LogLevel::Trace));
        assert_eq!(LogLevel::ALL.last(), Some(&LogLevel::Error));
    }

    /// The arrays name every variant once, in the spelling the record carries.
    #[test]
    fn every_level_and_source_is_listed_once_by_its_record_name() {
        assert_eq!(
            LogLevel::ALL.map(|level| level.as_str()),
            ["trace", "debug", "info", "warn", "error"]
        );
        assert_eq!(
            Source::ALL.map(|source| source.as_str()),
            ["rust", "python"]
        );
        for level in LogLevel::ALL {
            assert_eq!(
                serde_json::to_string(&level).unwrap(),
                format!("\"{}\"", level.as_str())
            );
        }
        for source in Source::ALL {
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{}\"", source.as_str())
            );
        }
    }

    /// Parses the exact example line documented in `docs/logging-schema.md`.
    /// If this test fails, the published schema example and the
    /// implementation have drifted — fix one or the other.
    #[test]
    fn docs_example_line_parses() {
        let line = r#"{"schema_version":1,"host_ts":1700000000000000000,"runtime_id":"Rabc123","stream":"main","source":"rust","level":"info","message":"processor started","target":"streamlib_media_builtins::camera_source","pipeline_id":"pl-42","processor_id":"camera-1","rhi_op":null,"intercepted":false,"attrs":{"device":"/dev/video0"}}"#;
        let ev: RuntimeLogEvent = serde_json::from_str(line).expect("docs example must parse");
        assert_eq!(ev.schema_version, SCHEMA_VERSION);
        assert_eq!(ev.runtime_id, "Rabc123");
        assert_eq!(ev.stream.as_deref(), Some("main"));
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

    /// `stream` is additive: a record written before it existed still parses, as
    /// one no stream emitted.
    #[test]
    fn a_record_carrying_no_stream_parses_as_one_no_stream_emitted() {
        let line = r#"{"schema_version":1,"host_ts":1,"runtime_id":"Rabc","source":"rust","level":"info","message":"m","target":"t","intercepted":false}"#;
        let ev: RuntimeLogEvent =
            serde_json::from_str(line).expect("a record without a stream parses");
        assert_eq!(ev.stream, None);
        assert!(
            !serde_json::to_string(&ev).unwrap().contains("\"stream\""),
            "a record no stream emitted carries no stream key"
        );
    }
}
