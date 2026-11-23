// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the CLI reads of `describe_runtime`'s answer. The CLI calls it with no contract
//! handshake, so it reads only the keys every runtime has answered since the tool was added,
//! and a runtime built before a later key was added still describes itself.

use std::path::PathBuf;

use serde::Deserialize;

use crate::project_log_records_from_the_runtime::DESCRIBE_RUNTIME_TOOL_NAME;
use crate::stream_actions_on_the_runtime::tool_result_from;

/// The keys of `DescribeRuntimeToolResult` the CLI reads; every other key is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct RuntimeDescriptionTheCliReads {
    /// The runtime's crate version.
    pub(crate) version: String,
    /// `<crate version>+<git sha>.<per-build nonce>`.
    pub(crate) build_id: String,
    /// Monotonic milliseconds since the runtime's engine started.
    pub(crate) up_for_ms: u64,
    /// The folder the runtime's own log is written to; `None` when it keeps none.
    pub(crate) runtime_log_directory: Option<PathBuf>,
}

impl RuntimeDescriptionTheCliReads {
    /// The description `describe_runtime` answered as `tool_result_text`, or the refusal naming
    /// what it answered instead.
    pub(crate) fn from_the_tool_result_text(tool_result_text: &str) -> Result<Self, String> {
        tool_result_from(DESCRIBE_RUNTIME_TOOL_NAME, tool_result_text)
    }
}
