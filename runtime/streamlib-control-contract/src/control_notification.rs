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
    /// The connection fell behind and missed changes.
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
}

/// `project_changed`: one change on a subscribed project's feed.
pub struct ProjectChanged;

impl ControlNotification for ProjectChanged {
    type Params = ProjectChangedNotificationParams;
    const METHOD_NAME: &'static str = "project_changed";
    const ONE_LINE_DOC: &'static str = "A change on the feed of a project the connection subscribed to: a transaction applied, an action refused, or media facts read.";
    const SUBSCRIBED_BY_METHOD_NAME: &'static str = "subscribe_project_changes";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const EXPOSURES: &'static [ControlOperationExposure] = &[
        ControlOperationExposure::Studio,
        ControlOperationExposure::Public,
    ];
}

/// Every notification the contract declares.
pub const CONTROL_NOTIFICATIONS: &[ControlNotificationDeclaration] =
    &[ControlNotificationDeclaration::of::<ProjectChanged>()];

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
