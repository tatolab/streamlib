// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The results of the operations the contract itself defines, rather than the action contract.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What `discover` answers: the contract a runtime serves and what it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiscoverResult {
    /// The contract version this runtime serves.
    pub contract_version: u32,
    /// The runtime's crate version.
    pub runtime_version: String,
    /// The runtime's build id, `<version>+<git sha>.<per-build nonce>`.
    pub build_id: String,
    /// Every method this runtime serves, in registry order.
    pub operations: Vec<String>,
}

/// What `subscribe_project_changes` answers: the projects the connection is now sent changes of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeProjectChangesResult {
    /// Every project the connection is subscribed to, by UUID, this call's among them.
    pub projects: Vec<Uuid>,
}

/// What `subscribe_stream_graph_changes` answers: the streams the connection is now sent graph
/// changes of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeStreamGraphChangesResult {
    /// Every stream the connection is subscribed to, by stream id, this call's among them.
    pub streams: Vec<String>,
}

/// What `subscribe_project_log_records` answers: the projects the connection is now told of new
/// log records of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeProjectLogRecordsResult {
    /// Every project the connection is subscribed to, by its directory as the connection spelled
    /// it, this call's among them.
    pub project_directories: Vec<std::path::PathBuf>,
}

/// What `subscribe_stream_action_changes` answers: the actions under way when the subscription
/// began, which the client describes once to follow from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeStreamActionChangesResult {
    /// The id of each stream action queued or running as the subscription began.
    pub actions_under_way: Vec<String>,
}

/// The `data` of a `discover` refused for a client built against another contract version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ControlContractVersionMismatch {
    /// The version the client sent.
    pub client_contract_version: u32,
    /// The version this runtime serves.
    pub runtime_contract_version: u32,
}

impl std::fmt::Display for ControlContractVersionMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the client was built against control contract version {}, and this runtime serves version {}",
            self.client_contract_version, self.runtime_contract_version
        )
    }
}
