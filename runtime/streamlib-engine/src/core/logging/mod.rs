// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified logging pathway: `tracing` → bounded lossy channel → drain
//! worker → line-buffered pretty stdout mirror + batched JSONL file.
//!
//! See `docs/logging-schema.md` for the JSONL schema (the durable
//! interface contract) and `CLAUDE.md` for the engine-model framing.

pub use config::{LoggingTunables, StreamlibLoggingConfig};
pub use event::{LogLevel, RuntimeLogEvent, SCHEMA_VERSION, Source};
pub use init::{StreamlibLoggingGuard, init, init_for_tests};
pub use paths::{log_dir, runtime_log_path};
pub(crate) use polyglot_sink::push_polyglot_record;
pub(crate) use record::LogRecord;
pub(crate) use worker::now_ns;

/// Emit one record from the app's own interpreter into the unified JSONL
/// pipeline, carrying caller-supplied dynamic attrs.
///
/// Bypasses `tracing::event!` deliberately: the macro cannot carry a
/// runtime-shaped attr map, and routing through the polyglot sink keeps
/// `source: python` honest in the JSONL columns (same reasoning as the
/// subprocess log relay in `polyglot_sink`). Silently no-ops before
/// [`init`] runs, matching `tracing::*!()` behaviour.
pub fn emit_app_process_python_log_record(
    level: LogLevel,
    message: String,
    attrs: std::collections::BTreeMap<String, serde_json::Value>,
) {
    push_polyglot_record(LogRecord {
        host_ts: now_ns(),
        level,
        target: "streamlib::python".to_string(),
        message,
        pipeline_id: None,
        // No processor: every Python processor runs in its own child, and a
        // child's records arrive attributed through the escalate `Log` op.
        processor_id: None,
        rhi_op: None,
        intercepted: false,
        channel: None,
        attrs,
        source: Some(Source::Python),
        source_ts: None,
        source_seq: None,
    });
}

mod config;
mod event;
pub(crate) mod iceoryx2_log_bridge;
mod init;
mod layer;
mod paths;
mod polyglot_sink;
mod record;
#[cfg(unix)]
mod stdio_interceptor;
mod worker;
mod writer;

#[cfg(test)]
mod tests;
