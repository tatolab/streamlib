// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified logging pathway: `tracing` → bounded lossy channel → drain
//! worker → line-buffered pretty mirror + the batched JSONL log of the
//! project whose stream or stream action emitted each record.
//!
//! See `docs/logging-schema.md` for the JSONL schema (the durable
//! interface contract) and `CLAUDE.md` for the engine-model framing.

pub use config::{LoggingTunables, PrettyLogMirrorStandardStream, StreamlibLoggingConfig};
pub use helper_process_engine_log_capture::{
    EngineLogRecordForTheParentProcess, EngineLogRecordsDrainedForTheParentProcess,
    HelperProcessEngineLogRecordRing, capture_this_helper_processes_engine_log_records,
};
pub(crate) use iceoryx2_log_bridge::install_iceoryx2_log_bridge_at_the_engines_configured_level;
pub(crate) use init::request_a_best_effort_flush;
pub use init::{
    ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES, ProcessLoggingPathwayHold, StreamlibLoggingGuard,
    hold_the_process_logging_pathway, init_for_tests,
};
pub(crate) use polyglot_sink::push_polyglot_record;
pub use project_log_route::RUNTIME_OWN_LOG_INSTANCE_NAME;
pub(crate) use project_log_route::TheRuntimesOwnLogWhileItsEngineLives;
pub use project_log_route::{
    ProjectLogRoute, ProjectLogRouteEntry, ProjectLogRouteEntryEnteredOnThisThread,
    carrying_this_threads_project_log_route_entry,
    run_in_the_project_log_route_entry_when_there_is_one,
    the_project_log_route_entry_of_this_thread,
};
pub(crate) use project_log_route::{
    carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe,
    polled_in_a_project_log_route_entry,
};
pub(crate) use record::LogRecord;

mod config;
mod helper_process_engine_log_capture;
pub(crate) mod iceoryx2_log_bridge;
mod init;
mod layer;
mod polyglot_sink;
mod project_log_route;
mod record;
#[cfg(unix)]
mod stdio_interceptor;
mod worker;
mod writer;

#[cfg(test)]
pub(crate) mod one_stream_log_file_written_on_a_test_thread;
#[cfg(test)]
mod tests;
