// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified logging pathway: `tracing` → bounded lossy channel → drain
//! worker → line-buffered pretty stdout mirror + batched JSONL file.
//!
//! See `docs/logging-schema.md` for the JSONL schema (the durable
//! interface contract) and `CLAUDE.md` for the engine-model framing.

pub use config::{LoggingTunables, StreamlibLoggingConfig};
pub use event::{LogLevel, RuntimeLogEvent, Source, SCHEMA_VERSION};
pub use init::{init, init_for_tests, StreamlibLoggingGuard};
pub use paths::{log_dir, runtime_log_path};
pub(crate) use polyglot_sink::push_polyglot_record;
pub(crate) use record::LogRecord;

mod config;
mod event;
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
