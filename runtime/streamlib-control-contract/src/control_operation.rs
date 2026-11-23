// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What every control operation declares about itself, and the row of the registry table it
//! becomes.

use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One operation of the control plane: the request it takes, the result it answers, and what the
/// contract file says about it.
pub trait ControlOperation {
    /// The params a caller sends.
    type Request: DeserializeOwned + JsonSchema;
    /// What the operation answers when it is not refused.
    type Result: Serialize + DeserializeOwned + JsonSchema;

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
    /// What it does to what the runtime holds.
    const EFFECT: ControlOperationEffect;
    /// How the agent surface offers it.
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing;
    /// Whether it answers only on a connection that stays open, `/control/ws` or `/mcp/stdio`,
    /// because what it opens lasts as long as that connection; a request answered in its own
    /// response is refused it.
    const NEEDS_A_CONNECTION_THAT_STAYS_OPEN: bool = false;
    /// The method name of the operation that replaces it, for one an agent no longer finds
    /// listed though it still answers.
    const REPLACED_BY: Option<&'static str> = None;
    /// A worked example of a call, which the discovery tools and the reference docs show; one
    /// the discovery tools list carries one.
    const EXAMPLE: Option<ControlOperationExample> = None;
}

/// A worked example of calling one operation, rendered into the contract file, `describe_operations`
/// and the reference docs from this one source.
#[derive(Clone, Copy)]
pub struct ControlOperationExample {
    /// The params a call sends.
    pub params: fn() -> Value,
    /// What that call answers; given for an operation the three agent loops call.
    pub answer: Option<fn() -> Value>,
    /// A refusal of a call of it, in the one refusal shape; given for an operation the three
    /// agent loops call.
    pub refusal: Option<fn() -> Value>,
}

/// What an operation does to what the runtime holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperationEffect {
    /// It changes nothing a project or a stream holds.
    ReadsOnly,
    /// It adds to or changes what a project or a stream holds.
    Changes,
    /// It can take away or overwrite what a project or a stream holds.
    Destroys,
}

/// How the agent surface offers an operation it serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperationAgentToolListing {
    /// It is a tool of its own in the agent surface's tool list.
    ListedAsATool,
    /// It is reached through the agent surface's `call_operation` tool.
    ReachedThroughCallOperation,
}

/// Whether an operation's shape is settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperationStability {
    /// Changing it breaks the contract and bumps its version.
    Stable,
    /// It may change before it turns stable, with no version bump.
    Experimental,
}

/// The reference docs' group an operation is listed under.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
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
    /// What it does to what the runtime holds.
    pub effect: ControlOperationEffect,
    /// How the agent surface offers it.
    pub agent_tool_listing: ControlOperationAgentToolListing,
    /// Whether it answers only on a connection that stays open.
    pub needs_a_connection_that_stays_open: bool,
    /// The method name of the operation that replaces it.
    pub replaced_by: Option<&'static str>,
    /// Its worked example.
    pub example: Option<ControlOperationExample>,
    /// Whether a value reads back as its result, and serializes again to the same value.
    pub reads_back_as_its_result: fn(&Value) -> Result<(), String>,
    /// The schema of its request, its named types added to the generator's definitions.
    pub request_json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
    /// The schema of its result, its named types added to the generator's definitions.
    pub result_json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
}

impl ControlOperationDeclaration {
    /// Whether the agent surface serves it.
    pub fn is_served_to_agents(&self) -> bool {
        self.exposures.contains(&ControlOperationExposure::Agent)
    }

    /// Whether the discovery tools list it: the agent surface serves it and no operation
    /// replaces it.
    pub fn is_listed_by_the_discovery_tools(&self) -> bool {
        self.is_served_to_agents() && self.replaced_by.is_none()
    }

    /// Whether it is a tool of its own in the agent surface's tool list.
    pub fn is_listed_as_an_agent_tool(&self) -> bool {
        self.is_served_to_agents()
            && self.agent_tool_listing == ControlOperationAgentToolListing::ListedAsATool
    }

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
            effect: Operation::EFFECT,
            agent_tool_listing: Operation::AGENT_TOOL_LISTING,
            needs_a_connection_that_stays_open: Operation::NEEDS_A_CONNECTION_THAT_STAYS_OPEN,
            replaced_by: Operation::REPLACED_BY,
            example: Operation::EXAMPLE,
            reads_back_as_its_result: the_value_reads_back_as::<Operation::Result>,
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

fn the_value_reads_back_as<Read: Serialize + DeserializeOwned>(
    value: &Value,
) -> Result<(), String> {
    let read: Read = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    let written = serde_json::to_value(&read).map_err(|error| error.to_string())?;
    if every_field_is_written_back(value, &written) {
        Ok(())
    } else {
        Err(format!("it reads back as {written}"))
    }
}

/// Whether `written` holds each field of `value` as `value` states it: a field `value` leaves
/// out may be written with its default, and one it states that the type does not read is lost.
pub(crate) fn every_field_is_written_back(value: &Value, written: &Value) -> bool {
    match (value, written) {
        (Value::Object(fields), Value::Object(written_fields)) => {
            fields.iter().all(|(key, field)| {
                written_fields
                    .get(key)
                    .is_some_and(|written_field| every_field_is_written_back(field, written_field))
            })
        }
        (Value::Array(items), Value::Array(written_items)) => {
            items.len() == written_items.len()
                && items
                    .iter()
                    .zip(written_items)
                    .all(|(item, written_item)| every_field_is_written_back(item, written_item))
        }
        _ => value == written,
    }
}
