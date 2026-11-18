// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// What a function node the helper's islands call raised, or `null` once its
/// calls run cleanly again: the graph shows it on the function node while the
/// reader keeps running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestSetFunctionNodeRaisingBecause {
    /// The function node's id, as the island the helper was handed names it.
    pub(crate) function_node: String,

    /// What its call raised, as the graph shows it; `null` for a clean call.
    pub(crate) raising_because: Option<String>,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
