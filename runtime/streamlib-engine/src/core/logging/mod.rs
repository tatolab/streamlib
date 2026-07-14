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
pub use worker::format_event_pretty;
pub(crate) use worker::now_ns;

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
