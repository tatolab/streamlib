// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified logging pathway: `tracing` → bounded lossy channel → drain
//! worker → line-buffered pretty mirror + the batched JSONL file of the
//! stream that emitted each record.
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
pub use loaded_stream_log_record_history::{
    LOADED_STREAM_LOG_RECORDS_HELD_IN_MEMORY, LoadedStreamLogRecordsPage, NumberedLogRecord,
};
pub use loaded_stream_log_route::RUNTIME_OWN_LOG_INSTANCE_NAME;
pub(crate) use loaded_stream_log_route::TheRuntimesOwnLogWhileItsEngineLives;
pub use loaded_stream_log_route::{
    LoadedStreamLogRoute, LoadedStreamLogRouteEnteredOnThisThread,
    carrying_this_threads_loaded_stream_log_route,
    run_in_the_loaded_stream_log_route_when_there_is_one,
    the_loaded_stream_log_route_of_this_thread,
};
pub(crate) use loaded_stream_log_route::{
    carrying_this_threads_loaded_stream_log_route_and_reading_a_helper_pipe,
    polled_in_a_loaded_stream_log_route,
};
pub(crate) use polyglot_sink::push_polyglot_record;
pub(crate) use record::LogRecord;

mod config;
mod helper_process_engine_log_capture;
pub(crate) mod iceoryx2_log_bridge;
mod init;
mod layer;
mod loaded_stream_log_record_history;
mod loaded_stream_log_route;
mod polyglot_sink;
mod record;
#[cfg(unix)]
mod stdio_interceptor;
mod worker;
mod writer;

#[cfg(test)]
pub(crate) mod one_stream_log_file_written_on_a_test_thread;
#[cfg(test)]
mod tests;
