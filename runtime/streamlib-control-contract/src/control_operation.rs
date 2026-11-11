// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every control operation declares about itself, and the row of the registry table it
//! becomes.

use schemars::{JsonSchema, SchemaGenerator};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// One operation of the control plane: the request it takes, the result it answers, and what the
/// contract file says about it.
pub trait ControlOperation {
    /// The params a caller sends.
    type Request: DeserializeOwned + JsonSchema;
    /// What the operation answers when it is not refused.
    type Result: Serialize + JsonSchema;

    /// The method name every adapter serves it under.
    const METHOD_NAME: &'static str;
    /// One sentence saying what it does, for the reference docs and the SDK's doc comments.
    const ONE_LINE_DOC: &'static str;
    /// How an agent uses it: what it reads, what it changes and what it answers.
    const AGENT_USAGE_NOTE: &'static str;
    /// The contract version it first appeared in.
    const SINCE_CONTRACT_VERSION: u32;
    /// Whether it may still change without a contract version bump.
    const STABILITY: ControlOperationStability;
    /// The reference docs' group it is listed under.
    const GROUP: ControlOperationGroup;
    /// Which surfaces serve it.
    const EXPOSURES: &'static [ControlOperationExposure];
}

/// Whether an operation's shape is settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperationStability {
    /// Changing it breaks the contract and bumps its version.
    Stable,
    /// It may change before it turns stable, with no version bump.
    Experimental,
}

/// The reference docs' group an operation is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperationGroup {
    /// A project as a whole.
    Project,
    /// The streams a runtime holds.
    Stream,
    /// The edits of a stream's graph.
    Graph,
    /// A project's assets.
    Asset,
    /// A project's other files.
    File,
    /// A project's imports of other projects.
    Dependency,
    /// The node types a project offers.
    NodeType,
    /// The Tato Studio views a project brings.
    View,
    /// Reading what a runtime and its streams are doing.
    Observe,
}

/// A surface an operation is served on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperationExposure {
    /// Tato Studio's generated client.
    Studio,
    /// The agent surface over MCP.
    Agent,
    /// The public SDKs and reference docs.
    Public,
}

/// One row of the registry table: an operation's declaration with its schemas reachable from a
/// generator.
#[derive(Clone, Copy)]
pub struct ControlOperationDeclaration {
    /// The method name.
    pub method_name: &'static str,
    /// The one-line doc.
    pub one_line_doc: &'static str,
    /// The agent usage note.
    pub agent_usage_note: &'static str,
    /// The contract version it first appeared in.
    pub since_contract_version: u32,
    /// Its stability.
    pub stability: ControlOperationStability,
    /// Its group.
    pub group: ControlOperationGroup,
    /// The surfaces serving it.
    pub exposures: &'static [ControlOperationExposure],
    /// The schema of its request, its named types added to the generator's definitions.
    pub request_json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
    /// The schema of its result, its named types added to the generator's definitions.
    pub result_json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
}

impl ControlOperationDeclaration {
    /// The table row of `Operation`.
    pub const fn of<Operation: ControlOperation>() -> Self {
        Self {
            method_name: Operation::METHOD_NAME,
            one_line_doc: Operation::ONE_LINE_DOC,
            agent_usage_note: Operation::AGENT_USAGE_NOTE,
            since_contract_version: Operation::SINCE_CONTRACT_VERSION,
            stability: Operation::STABILITY,
            group: Operation::GROUP,
            exposures: Operation::EXPOSURES,
            request_json_schema: the_json_schema_of::<Operation::Request>,
            result_json_schema: the_json_schema_of::<Operation::Result>,
        }
    }
}

fn the_json_schema_of<Described: JsonSchema>(
    schema_generator: &mut SchemaGenerator,
) -> schemars::Schema {
    schema_generator.subschema_for::<Described>()
}
