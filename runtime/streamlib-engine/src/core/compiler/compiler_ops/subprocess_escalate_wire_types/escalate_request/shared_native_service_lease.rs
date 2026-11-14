// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// Lease a native service the runtime's host offers every node, starting it
/// when no node holds a lease on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquireSharedNativeServiceLease {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The name the host offered the service under, such as `browser-host`.
    pub(crate) service_name: String,
}

/// Ask whether a leased service is there to connect to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestReadSharedNativeServiceLeaseState {
    /// The `handle_id` the lease was granted under.
    pub(crate) lease_id: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}

/// Give a lease back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestReleaseSharedNativeServiceLease {
    /// The `handle_id` the lease was granted under.
    pub(crate) lease_id: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
