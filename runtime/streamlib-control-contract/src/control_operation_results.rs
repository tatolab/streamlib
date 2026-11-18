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
    /// The contract revision this runtime serves: the additions made within its version.
    pub contract_revision: u32,
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

/// What `subscribe_input_value_changes` answers: the streams the connection is now sent input
/// value changes of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeInputValueChangesResult {
    /// Every stream the connection is subscribed to the input value changes of, by stream id,
    /// this call's among them.
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

/// Which side of a refused connection was built against the older contract, and so is the one
/// to update.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OlderSide {
    /// The runtime serves an older contract than the client was built against.
    Runtime,
    /// The client was built against an older contract than the runtime serves.
    Client,
}

/// The `data` of a call, or the body of a WebSocket upgrade, refused for a client built against
/// another contract than the runtime serves: another version, or a higher revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ControlContractVersionMismatch {
    /// The version the client sent.
    pub client_contract_version: u32,
    /// The version this runtime serves.
    pub runtime_contract_version: u32,
    /// The revision the client sent, 0 when it sent none.
    pub client_contract_revision: u32,
    /// The revision this runtime serves.
    pub runtime_contract_revision: u32,
    /// The side built against the older contract, the one to update.
    pub older_side: OlderSide,
    /// The runtime's release, for the words a person reads.
    pub runtime_version: String,
}

impl std::fmt::Display for ControlContractVersionMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let which_is_older = match self.older_side {
            OlderSide::Runtime => "the runtime is older than the client; update the runtime",
            OlderSide::Client => {
                "the client is older than the runtime; take the client the runtime carries"
            }
        };
        write!(
            formatter,
            "the client was built against control contract version {} revision {}, and this \
             runtime, release {}, serves version {} revision {}: {which_is_older}",
            self.client_contract_version,
            self.client_contract_revision,
            self.runtime_version,
            self.runtime_contract_version,
            self.runtime_contract_revision,
        )
    }
}
