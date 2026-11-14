// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// What the helper's node's `process()` raised, or `null` once it runs cleanly
/// again: the graph shows it beside the node's state while the node keeps
/// running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestSetProcessRaisingBecause {
    /// What `process()` raised, as the graph shows it; `null` for a clean run.
    pub(crate) raising_because: Option<String>,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
