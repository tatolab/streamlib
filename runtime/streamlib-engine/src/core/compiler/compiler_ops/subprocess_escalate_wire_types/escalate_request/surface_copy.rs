// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// Copy one surface's pixels into another, same format and extent. The host
/// records, submits and waits before it answers, so the reply means the
/// destination's next reader sees the copied pixels; no timeline value
/// crosses back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestCopySurfaceToSurface {
    /// Any surface id the engine can resolve: a published frame
    /// (`<slot>#<generation>`), a pool slot, or a registered texture's id.
    pub(crate) destination_surface_id: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Any surface id the engine can resolve, as `destination_surface_id`.
    pub(crate) source_surface_id: String,
}
