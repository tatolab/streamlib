// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified logging pathway: `tracing` → bounded lossy channel → drain
//! worker → line-buffered pretty stdout mirror + batched JSONL file.
//!
//! See `docs/logging-schema.md` for the JSONL schema (the durable
//! interface contract) and `CLAUDE.md` for the engine-model framing.

pub use config::{LoggingTunables, StreamlibLoggingConfig};
pub use event::{LogLevel, RuntimeLogEvent, Source, SCHEMA_VERSION};
pub use init::{init, StreamlibLoggingGuard};
pub use paths::{log_dir, runtime_log_path};

mod config;
mod event;
mod init;
mod layer;
mod paths;
mod record;
mod worker;
mod writer;

#[cfg(test)]
mod tests;
