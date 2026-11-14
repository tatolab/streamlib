// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestReleaseHandle {
    /// Opaque handle ID previously returned by acquire_*.
    pub(crate) handle_id: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
