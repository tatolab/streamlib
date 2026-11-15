// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The `logs` tool's result: one project's log records after the cursor asked for, read from
/// the project's log on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LogsToolResult {
    /// The project whose log these are.
    pub project_directory: PathBuf,
    /// The records after `after` that match the call's `stream` and `action`, oldest first, each
    /// as the project's JSONL log holds it.
    pub records: Vec<serde_json::Value>,
    /// The opaque cursor the next call passes as `after` to read on from this page.
    pub next_after: String,
    /// Whether retention deleted the segment `after` named, so the page resumes at the oldest
    /// record held.
    pub records_no_longer_held: bool,
}
