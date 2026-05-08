// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Intermediate record pushed onto the drain channel. Two producers enqueue
//! records here: the [`JsonlSinkLayer`] (tracing events from local call
//! sites) and [`push_polyglot_record`] (records relayed from Python/Deno
//! subprocesses via escalate IPC). Owns all its strings — we can't borrow
//! from `tracing::Event` across the channel send.
//!
//! [`JsonlSinkLayer`]: crate::core::logging::layer::JsonlSinkLayer
//! [`push_polyglot_record`]: crate::core::logging::push_polyglot_record

use std::collections::BTreeMap;

use crate::core::logging::event::{LogLevel, Source};

/// Record pushed onto the drain channel. Owned strings.
#[derive(Debug, Clone)]
pub(crate) struct LogRecord {
    pub host_ts: u64,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub pipeline_id: Option<String>,
    pub processor_id: Option<String>,
    pub rhi_op: Option<String>,
    pub intercepted: bool,
    pub channel: Option<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    /// Override the worker's default [`Source`] for this record. `None`
    /// means "inherit the worker's configured source" (i.e. `Rust` for
    /// local tracing events). `Some(Python)` / `Some(Deno)` is used by
    /// records that originate in a subprocess and arrive via escalate IPC.
    pub source: Option<Source>,
    /// Subprocess wall-clock timestamp ISO8601. Advisory — never used for
    /// ordering. `None` for local tracing records.
    pub source_ts: Option<String>,
    /// Subprocess-monotonic sequence number. Escape hatch for recovering
    /// per-source order. `None` for local tracing records.
    pub source_seq: Option<u64>,
}
