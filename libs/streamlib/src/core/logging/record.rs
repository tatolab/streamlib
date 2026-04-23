// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Intermediate record pushed onto the channel by the tracing layer and
//! drained by the worker. Owns all its strings — we can't borrow from
//! `tracing::Event` across the channel send.

use std::collections::BTreeMap;

use crate::core::logging::event::LogLevel;

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
    pub attrs: BTreeMap<String, serde_json::Value>,
}
