// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The notifications a runtime sends a subscribed connection, each declared once as the
//! operations are.

use schemars::{JsonSchema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectTransactionOnTheWire;
use uuid::Uuid;

use crate::control_operation::{ControlOperationExposure, ControlOperationStability};

/// One notification the runtime sends: its method, its params, and what the contract file says
/// about it.
pub trait ControlNotification {
    /// The params it carries.
    type Params: Serialize + JsonSchema;

    /// The method name it is sent under.
    const METHOD_NAME: &'static str;
    /// One sentence saying what it tells of.
    const ONE_LINE_DOC: &'static str;
    /// The operation whose call starts it being sent.
    const SUBSCRIBED_BY_METHOD_NAME: &'static str;
    /// The contract version it first appeared in.
    const SINCE_CONTRACT_VERSION: u32;
    /// Whether it may still change without a contract version bump.
    const STABILITY: ControlOperationStability;
    /// Which surfaces send it.
    const EXPOSURES: &'static [ControlOperationExposure];
}

/// One row of the notification table.
#[derive(Clone, Copy)]
pub struct ControlNotificationDeclaration {
    /// The method name.
    pub method_name: &'static str,
    /// The one-line doc.
    pub one_line_doc: &'static str,
    /// The operation that subscribes to it.
    pub subscribed_by_method_name: &'static str,
    /// The contract version it first appeared in.
    pub since_contract_version: u32,
    /// Its stability.
    pub stability: ControlOperationStability,
    /// The surfaces sending it.
    pub exposures: &'static [ControlOperationExposure],
    /// The schema of its params, its named types added to the generator's definitions.
    pub params_json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
}

impl ControlNotificationDeclaration {
    /// The table row of `Notification`.
    pub const fn of<Notification: ControlNotification>() -> Self {
        Self {
            method_name: Notification::METHOD_NAME,
            one_line_doc: Notification::ONE_LINE_DOC,
            subscribed_by_method_name: Notification::SUBSCRIBED_BY_METHOD_NAME,
            since_contract_version: Notification::SINCE_CONTRACT_VERSION,
            stability: Notification::STABILITY,
            exposures: Notification::EXPOSURES,
            params_json_schema: the_json_schema_of::<Notification::Params>,
        }
    }
}

fn the_json_schema_of<Described: JsonSchema>(
    schema_generator: &mut SchemaGenerator,
) -> schemars::Schema {
    schema_generator.subschema_for::<Described>()
}

/// What changed on a project's change feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectChangeKindOnTheWire {
    /// A transaction applied and its history records it.
    Applied,
    /// The runtime refused an action on the project, which no history records.
    Refused,
    /// The runtime finished reading an asset's media facts.
    MediaFactsRead,
    /// Files in the project's folder changed, appeared or went away, an editor's saves and a
    /// transaction's own writes alike; `files` names them.
    FilesChanged,
    /// The connection fell behind and missed changes, or files changed in the project's folder
    /// that the OS did not name; the client reads the change feed and lists the project again.
    Missed,
}

/// `project_changed`'s params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectChangedNotificationParams {
    /// The project, by UUID.
    pub project: Uuid,
    /// What changed; `missed` when the connection fell behind, and the client reads the project's
    /// change feed again.
    pub change: ProjectChangeKindOnTheWire,
    /// The transaction that applied, when `change` is `applied`; its `undo_state` is absent.
    pub transaction: Option<ProjectTransactionOnTheWire>,
    /// The files that changed, by path relative to the project with `/` between segments, when
    /// `change` is `files_changed`; a hidden folder, `node_modules`, `__pycache__` and a Python
    /// virtual environment are never named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// `project_changed`: one change on a subscribed project's feed.
pub struct ProjectChanged;

impl ControlNotification for ProjectChanged {
    type Params = ProjectChangedNotificationParams;
    const METHOD_NAME: &'static str = "project_changed";
    const ONE_LINE_DOC: &'static str = "A change on the feed of a project the connection subscribed to: a transaction applied, an action refused, media facts read, or files in its folder changed.";
    const SUBSCRIBED_BY_METHOD_NAME: &'static str = "subscribe_project_changes";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const EXPOSURES: &'static [ControlOperationExposure] = &[
        ControlOperationExposure::Studio,
        ControlOperationExposure::Public,
    ];
}

/// What changed in a loaded stream's live graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreamGraphChangeKindOnTheWire {
    /// A node or a link was added, removed or reconfigured.
    Edited,
    /// One node's state changed, named by `node` and `node_state`.
    NodeStateChanged,
    /// The stream's playback started, stopped, paused or resumed, or failed to.
    PlaybackStateChanged,
    /// A node's dropped or refused bag counts, the raise its `process()` keeps, or a link's state
    /// changed; sent at most once every 500 ms per stream, and the client reads the graph again.
    DropsRaisesOrLinkStatesChanged,
    /// The connection missed changes, or the stream was loaded again or unloaded, and the client
    /// reads the graph again.
    Missed,
}

/// A node's state as `graph` renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NodeStateOnTheWire {
    /// Waiting to be started.
    Pending,
    /// Prepared, its `setup` not yet run.
    Idle,
    /// Running.
    Running,
    /// Paused with its stream.
    Paused,
    /// Shutting down.
    Stopping,
    /// Stopped.
    Stopped,
    /// Failed; `graph` says why.
    Error,
    /// Its helper process died on its own and a new one starts after a backoff.
    Restarting,
}

/// `stream_graph_changed`'s params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamGraphChangedNotificationParams {
    /// The stream, by stream id.
    pub stream: String,
    /// What changed; `missed` when the client reads the graph again.
    pub change: StreamGraphChangeKindOnTheWire,
    /// The node whose state changed, by its graph `id`, when `change` is `node_state_changed`.
    pub node: Option<String>,
    /// The node's new state, when `change` is `node_state_changed`.
    pub node_state: Option<NodeStateOnTheWire>,
}

/// `stream_graph_changed`: one change in the live graph of a stream the connection subscribed to.
pub struct StreamGraphChanged;

impl ControlNotification for StreamGraphChanged {
    type Params = StreamGraphChangedNotificationParams;
    const METHOD_NAME: &'static str = "stream_graph_changed";
    const ONE_LINE_DOC: &'static str = "A change in the live graph of a stream the connection subscribed to: an edit, a node's new state, a playback change, new drops, raises or link states, or `missed`.";
    const SUBSCRIBED_BY_METHOD_NAME: &'static str = "subscribe_stream_graph_changes";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const EXPOSURES: &'static [ControlOperationExposure] = &[
        ControlOperationExposure::Studio,
        ControlOperationExposure::Public,
    ];
}

/// What happened to the stream actions a connection follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionChangeKindOnTheWire {
    /// The action named by `action` changed.
    Changed,
    /// The connection missed changes, and the client describes each action it follows again.
    Missed,
}

/// `stream_action_changed`'s params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamActionChangedNotificationParams {
    /// What happened; `missed` when the client calls `describe_stream_action` for each action it
    /// follows.
    pub change: StreamActionChangeKindOnTheWire,
    /// The action that changed, by id, when `change` is `changed`.
    pub action: Option<String>,
}

/// `stream_action_changed`: one stream action changed.
pub struct StreamActionChanged;

impl ControlNotification for StreamActionChanged {
    type Params = StreamActionChangedNotificationParams;
    const METHOD_NAME: &'static str = "stream_action_changed";
    const ONE_LINE_DOC: &'static str =
        "A stream action changed, which `describe_stream_action` describes, or `missed`.";
    const SUBSCRIBED_BY_METHOD_NAME: &'static str = "subscribe_stream_action_changes";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const EXPOSURES: &'static [ControlOperationExposure] = &[
        ControlOperationExposure::Studio,
        ControlOperationExposure::Public,
    ];
}

/// What a `project_log_records_written` tells of a project's log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLogRecordsChangeKindOnTheWire {
    /// The runtime wrote new records to the project's log.
    Written,
    /// The connection fell behind and may have missed a write.
    Missed,
}

/// `project_log_records_written`'s params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectLogRecordsWrittenNotificationParams {
    /// The project, by its directory as the connection spelled it to
    /// `subscribe_project_log_records`; once for each spelling of one project subscribed.
    pub project_directory: std::path::PathBuf,
    /// `written`, or `missed` when the connection fell behind; either way the client reads `logs`
    /// on from its cursor.
    pub change: ProjectLogRecordsChangeKindOnTheWire,
}

/// `project_log_records_written`: a subscribed project's log has new records to read.
pub struct ProjectLogRecordsWritten;

impl ControlNotification for ProjectLogRecordsWritten {
    type Params = ProjectLogRecordsWrittenNotificationParams;
    const METHOD_NAME: &'static str = "project_log_records_written";
    const ONE_LINE_DOC: &'static str = "A project the connection subscribed to has new log records, which `logs` reads on from its cursor, or `missed`.";
    const SUBSCRIBED_BY_METHOD_NAME: &'static str = "subscribe_project_log_records";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const EXPOSURES: &'static [ControlOperationExposure] = &[
        ControlOperationExposure::Studio,
        ControlOperationExposure::Public,
    ];
}

/// Every notification the contract declares.
pub const CONTROL_NOTIFICATIONS: &[ControlNotificationDeclaration] = &[
    ControlNotificationDeclaration::of::<ProjectChanged>(),
    ControlNotificationDeclaration::of::<StreamGraphChanged>(),
    ControlNotificationDeclaration::of::<StreamActionChanged>(),
    ControlNotificationDeclaration::of::<ProjectLogRecordsWritten>(),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_operations::CONTROL_OPERATIONS;

    #[test]
    fn every_notification_is_subscribed_to_by_a_registered_operation() {
        for declaration in CONTROL_NOTIFICATIONS {
            assert!(
                CONTROL_OPERATIONS
                    .iter()
                    .any(|operation| operation.method_name == declaration.subscribed_by_method_name),
                "`{}` names `{}`, which the registry does not hold",
                declaration.method_name,
                declaration.subscribed_by_method_name
            );
        }
    }
}
