// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// The helper's node failed in a way it cannot recover from: the graph shows
/// it in error with `reason`, and the rest of the stream keeps running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestFailTheNode {
    /// Why the node failed, as the graph shows it.
    pub(crate) reason: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
