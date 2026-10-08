// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified logging pathway: `tracing` → bounded lossy channel → drain
//! worker → line-buffered pretty stdout mirror + batched JSONL file.
//!
//! See `docs/logging-schema.md` for the JSONL schema (the durable
//! interface contract) and `CLAUDE.md` for the engine-model framing.

pub use config::{LoggingTunables, PrettyLogMirrorStandardStream, StreamlibLoggingConfig};
pub use event::{LogLevel, RuntimeLogEvent, SCHEMA_VERSION, Source};
pub use helper_process_engine_log_capture::{
    EngineLogRecordForTheParentProcess, EngineLogRecordsDrainedForTheParentProcess,
    HelperProcessEngineLogRecordRing, capture_this_helper_processes_engine_log_records,
};
pub(crate) use iceoryx2_log_bridge::install_iceoryx2_log_bridge_at_the_engines_configured_level;
pub use init::{
    ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES, StreamlibLoggingGuard, init, init_for_tests,
};
pub use paths::{log_dir, runtime_log_path};
pub(crate) use polyglot_sink::{push_polyglot_record, request_a_best_effort_flush};
pub(crate) use record::LogRecord;

mod config;
mod event;
mod helper_process_engine_log_capture;
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
