// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// Why the helper's node, running, is not ready, or `null` once it is: the
/// graph shows it beside the node's state while the helper keeps the node's
/// outputs flowing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestSetNotReadyBecause {
    /// Why the node is not ready, in its own words; `null` once it is.
    pub(crate) not_ready_because: Option<String>,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
