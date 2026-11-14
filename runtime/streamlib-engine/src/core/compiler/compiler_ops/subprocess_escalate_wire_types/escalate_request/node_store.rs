// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Read the value the helper's node keeps under `key` in its project's store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestReadNodeStoreKey {
    /// The key, one of the node's own.
    pub(crate) key: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}

/// Keep `value` under `key` in the helper's node's own keys of its project's store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestWriteNodeStoreKey {
    /// The key, one of the node's own.
    pub(crate) key: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Any JSON value.
    pub(crate) value: Value,
}

/// Stop keeping `key` in the helper's node's own keys of its project's store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRemoveNodeStoreKey {
    /// The key, one of the node's own.
    pub(crate) key: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
