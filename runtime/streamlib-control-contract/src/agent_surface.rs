// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The agent surface: the operations it lists as tools of their own, and the three discovery
//! tools that reach every other operation it serves.

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::control_operation::{
    ControlOperationDeclaration, ControlOperationEffect, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operations::CONTROL_OPERATIONS;

/// The name of the discovery tool that lists the groups and the operations in each.
pub const LIST_OPERATION_GROUPS_TOOL_NAME: &str = "list_operation_groups";

/// The name of the discovery tool that describes operations with their schemas.
pub const DESCRIBE_OPERATIONS_TOOL_NAME: &str = "describe_operations";

/// The name of the discovery tool that calls any operation the agent surface serves.
pub const CALL_OPERATION_TOOL_NAME: &str = "call_operation";

/// `list_operation_groups`' params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListOperationGroupsRequest {}

/// `list_operation_groups`' result.
#[derive(Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ListOperationGroupsResult {
    /// Every group holding an operation the agent surface serves.
    pub groups: Vec<AgentOperationGroup>,
}

/// One group of the operations the agent surface serves.
#[derive(Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AgentOperationGroup {
    /// The group.
    pub group: ControlOperationGroup,
    /// The group's operations, in the order the contract lists them.
    pub operations: Vec<AgentOperationInItsGroup>,
}

/// One operation as its group lists it.
#[derive(Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AgentOperationInItsGroup {
    /// The operation's name, as `call_operation` and `describe_operations` take it.
    pub name: String,
    /// One sentence saying what it does.
    pub summary: String,
    /// Whether it is also a tool of its own, callable without `call_operation`.
    pub listed_as_a_tool: bool,
}

/// `describe_operations`' params.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeOperationsRequest {
    /// The operations to describe, by name.
    #[schemars(
        description = "The operations to describe, by the names `list_operation_groups` lists. Send this or `group`."
    )]
    pub operations: Option<Vec<String>>,
    /// The group whose operations to describe.
    #[schemars(description = "Describe every operation of this group. Send this or `operations`.")]
    pub group: Option<ControlOperationGroup>,
}

/// `describe_operations`' result.
#[derive(Debug, PartialEq, Serialize, JsonSchema)]
pub struct DescribeOperationsResult {
    /// Each operation asked for, in the order the contract lists them.
    pub operations: Vec<AgentOperationDescribed>,
}

/// One operation described for an agent about to call it.
#[derive(Debug, PartialEq, Serialize, JsonSchema)]
pub struct AgentOperationDescribed {
    /// The operation's name.
    pub name: String,
    /// One sentence saying what it does.
    pub summary: String,
    /// What it reads, what it changes and what it answers.
    pub usage: String,
    /// Its group.
    pub group: ControlOperationGroup,
    /// The contract version it first appeared in.
    pub since_contract_version: u32,
    /// Whether its shape is settled.
    pub stability: ControlOperationStability,
    /// What it does to what the runtime holds.
    pub effect: ControlOperationEffect,
    /// The JSON Schema of the `params` `call_operation` takes for it.
    pub params_schema: Value,
    /// The JSON Schema of what it answers.
    pub result_schema: Value,
}

/// `call_operation`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallOperationRequest {
    /// The operation to call, by name.
    #[schemars(description = "The operation to call, by the name `list_operation_groups` lists.")]
    pub operation: String,
    /// The operation's params.
    #[schemars(
        description = "The operation's params, as the `params_schema` `describe_operations` answers for it states them. Omit for an operation that takes none."
    )]
    pub params: Option<Map<String, Value>>,
}

/// One discovery tool of the agent surface.
#[derive(Clone, Copy)]
pub struct AgentDiscoveryToolDeclaration {
    /// The tool's name.
    pub tool_name: &'static str,
    /// One sentence saying what it does.
    pub one_line_doc: &'static str,
    /// How an agent uses it.
    pub agent_usage_note: &'static str,
    /// What it does to what the runtime holds.
    pub effect: ControlOperationEffect,
    /// The schema of its params, its named types added to the generator's definitions.
    pub request_json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
    /// The schema of its result, when its result has one shape.
    pub result_json_schema: Option<fn(&mut SchemaGenerator) -> schemars::Schema>,
}

/// The agent surface's discovery tools, in the order its tool list holds them.
pub const AGENT_DISCOVERY_TOOLS: &[AgentDiscoveryToolDeclaration] = &[
    AgentDiscoveryToolDeclaration {
        tool_name: LIST_OPERATION_GROUPS_TOOL_NAME,
        one_line_doc: "List every operation the runtime serves, by group.",
        agent_usage_note: "List every operation this runtime serves, by group, each with its name and one sentence saying what it does. The tools in this tool list are the ones used most; every other operation — a project's assets, files, dependencies, node types and views, each single graph edit, redo, viewers — is reached through `call_operation`. Start here when no listed tool does what you need, then read the operation's params with `describe_operations`.",
        effect: ControlOperationEffect::ReadsOnly,
        request_json_schema: the_json_schema_of::<ListOperationGroupsRequest>,
        result_json_schema: Some(the_json_schema_of::<ListOperationGroupsResult>),
    },
    AgentDiscoveryToolDeclaration {
        tool_name: DESCRIBE_OPERATIONS_TOOL_NAME,
        one_line_doc: "Describe operations: what each does, the params it takes and what it answers.",
        agent_usage_note: "Describe operations by name, or every operation of one group: what each reads, changes and answers, the JSON Schema of its params and of its result, whether it only reads, and the contract version it arrived in. Call it before the first `call_operation` of an operation, and send the params exactly as its `params_schema` states them.",
        effect: ControlOperationEffect::ReadsOnly,
        request_json_schema: the_json_schema_of::<DescribeOperationsRequest>,
        result_json_schema: Some(the_json_schema_of::<DescribeOperationsResult>),
    },
    AgentDiscoveryToolDeclaration {
        tool_name: CALL_OPERATION_TOOL_NAME,
        one_line_doc: "Call any operation the runtime serves, by name.",
        agent_usage_note: "Call any operation `list_operation_groups` lists, by name, with its `params`. It answers exactly what the operation answers, and a refusal is the operation's own. An operation that is also a listed tool answers the same either way.",
        effect: ControlOperationEffect::Destroys,
        request_json_schema: the_json_schema_of::<CallOperationRequest>,
        result_json_schema: None,
    },
];

fn the_json_schema_of<Described: JsonSchema>(
    schema_generator: &mut SchemaGenerator,
) -> schemars::Schema {
    schema_generator.subschema_for::<Described>()
}

/// Every operation the agent surface serves, in the order the contract lists them.
pub fn the_operations_served_to_agents()
-> impl Iterator<Item = &'static ControlOperationDeclaration> {
    CONTROL_OPERATIONS
        .iter()
        .filter(|declaration| declaration.is_served_to_agents())
}

/// The operation the agent surface serves under `method_name`.
pub fn the_operation_served_to_agents_as(
    method_name: &str,
) -> Option<&'static ControlOperationDeclaration> {
    the_operations_served_to_agents().find(|declaration| declaration.method_name == method_name)
}

/// What `list_operation_groups` answers.
pub fn the_operation_groups_served_to_agents() -> ListOperationGroupsResult {
    let mut groups: Vec<AgentOperationGroup> = Vec::new();
    for declaration in the_operations_served_to_agents() {
        let operation = AgentOperationInItsGroup {
            name: declaration.method_name.to_owned(),
            summary: declaration.one_line_doc.to_owned(),
            listed_as_a_tool: declaration.is_listed_as_an_agent_tool(),
        };
        match groups
            .iter_mut()
            .find(|listed| listed.group == declaration.group)
        {
            Some(listed) => listed.operations.push(operation),
            None => groups.push(AgentOperationGroup {
                group: declaration.group,
                operations: vec![operation],
            }),
        }
    }
    groups.sort_by_key(|listed| listed.group);
    ListOperationGroupsResult { groups }
}

/// What `describe_operations` answers for `request`, or why it describes nothing, in words an
/// agent can act on.
pub fn the_operations_described_for(
    DescribeOperationsRequest { operations, group }: DescribeOperationsRequest,
) -> Result<DescribeOperationsResult, String> {
    let described: Vec<&ControlOperationDeclaration> = match (operations, group) {
        (Some(names), None) => {
            if let Some(unknown) = names
                .iter()
                .find(|name| the_operation_served_to_agents_as(name).is_none())
            {
                return Err(format!(
                    "{DESCRIBE_OPERATIONS_TOOL_NAME} failed: the runtime serves no operation \
                     named `{unknown}`; `{LIST_OPERATION_GROUPS_TOOL_NAME}` lists every one"
                ));
            }
            the_operations_served_to_agents()
                .filter(|declaration| names.iter().any(|name| name == declaration.method_name))
                .collect()
        }
        (None, Some(group)) => the_operations_served_to_agents()
            .filter(|declaration| declaration.group == group)
            .collect(),
        _ => {
            return Err(format!(
                "{DESCRIBE_OPERATIONS_TOOL_NAME} failed: send exactly one of `operations` and \
                 `group`"
            ));
        }
    };
    Ok(DescribeOperationsResult {
        operations: described
            .into_iter()
            .map(|declaration| AgentOperationDescribed {
                name: declaration.method_name.to_owned(),
                summary: declaration.one_line_doc.to_owned(),
                usage: declaration.agent_usage_note.to_owned(),
                group: declaration.group,
                since_contract_version: declaration.since_contract_version,
                stability: declaration.stability,
                effect: declaration.effect,
                params_schema: a_json_schema_carrying_its_own_definitions(
                    declaration.request_json_schema,
                ),
                result_schema: a_json_schema_carrying_its_own_definitions(
                    declaration.result_json_schema,
                ),
            })
            .collect(),
    })
}

/// The schema `json_schema` builds as one JSON Schema 2020-12 document: its own keywords at the
/// root, and the named types it refers to under `$defs`.
pub fn a_json_schema_carrying_its_own_definitions(
    json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
) -> Value {
    let mut schema_generator = SchemaSettings::draft2020_12().into_generator();
    let subschema = json_schema(&mut schema_generator).to_value();
    let mut definitions = schema_generator.take_definitions(true);
    let mut schema = match the_definition_a_schema_only_refers_to(&subschema, &definitions) {
        Some(definition_name) => {
            let definition = definitions.get(&definition_name).cloned();
            let reference = format!("\"{DEFINITIONS_REFERENCE_PREFIX}{definition_name}\"");
            let is_referred_to_again = definitions
                .values()
                .any(|definition| definition.to_string().contains(&reference));
            if !is_referred_to_again {
                definitions.remove(&definition_name);
            }
            definition.unwrap_or(subschema)
        }
        None => subschema,
    };
    if let (Value::Object(schema), false) = (&mut schema, definitions.is_empty()) {
        schema.insert("$defs".to_owned(), Value::Object(definitions));
    }
    schema
}

const DEFINITIONS_REFERENCE_PREFIX: &str = "#/$defs/";

fn the_definition_a_schema_only_refers_to(
    schema: &Value,
    definitions: &Map<String, Value>,
) -> Option<String> {
    let schema = schema.as_object()?;
    let [(keyword, Value::String(reference))] = schema.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let definition_name = reference.strip_prefix(DEFINITIONS_REFERENCE_PREFIX)?;
    (keyword == "$ref" && definitions.contains_key(definition_name))
        .then(|| definition_name.to_owned())
}

/// One tool in the agent surface's tool list.
#[derive(Debug, PartialEq)]
pub struct AgentToolListed {
    /// The tool's name.
    pub tool_name: &'static str,
    /// How an agent uses it.
    pub description: &'static str,
    /// What it does to what the runtime holds.
    pub effect: ControlOperationEffect,
    /// The JSON Schema of its arguments.
    pub input_schema: Value,
    /// The JSON Schema of its result, when its result is one JSON object shape.
    pub output_schema: Option<Value>,
}

/// The agent surface's tool list: each operation listed as a tool of its own, in the order the
/// contract lists them, then the discovery tools.
pub fn the_agent_tools_listed() -> Vec<AgentToolListed> {
    let operations_listed = the_operations_served_to_agents()
        .filter(|declaration| declaration.is_listed_as_an_agent_tool())
        .map(|declaration| AgentToolListed {
            tool_name: declaration.method_name,
            description: declaration.agent_usage_note,
            effect: declaration.effect,
            input_schema: a_json_schema_carrying_its_own_definitions(
                declaration.request_json_schema,
            ),
            output_schema: the_schema_of_a_result_that_is_one_json_object(
                declaration.result_json_schema,
            ),
        });
    let discovery_tools = AGENT_DISCOVERY_TOOLS
        .iter()
        .map(|declaration| AgentToolListed {
            tool_name: declaration.tool_name,
            description: declaration.agent_usage_note,
            effect: declaration.effect,
            input_schema: a_json_schema_carrying_its_own_definitions(
                declaration.request_json_schema,
            ),
            output_schema: declaration
                .result_json_schema
                .and_then(the_schema_of_a_result_that_is_one_json_object),
        });
    operations_listed.chain(discovery_tools).collect()
}

/// MCP takes an `outputSchema` only of `type: object`.
fn the_schema_of_a_result_that_is_one_json_object(
    result_json_schema: fn(&mut SchemaGenerator) -> schemars::Schema,
) -> Option<Value> {
    let schema = a_json_schema_carrying_its_own_definitions(result_json_schema);
    (schema.get("type") == Some(&Value::String("object".to_owned()))).then_some(schema)
}

/// The agent tool, and the arguments it takes, that reach `operation_name` with `params`: the
/// operation's own tool when it is listed, `call_operation` otherwise.
pub fn the_agent_tool_call_that_reaches(
    operation_name: &str,
    params: Map<String, Value>,
) -> (String, Map<String, Value>) {
    let is_reached_through_call_operation = the_operation_served_to_agents_as(operation_name)
        .is_some_and(|declaration| !declaration.is_listed_as_an_agent_tool());
    if !is_reached_through_call_operation {
        return (operation_name.to_owned(), params);
    }
    let mut call_operation_arguments = Map::new();
    call_operation_arguments.insert(
        "operation".to_owned(),
        Value::String(operation_name.to_owned()),
    );
    call_operation_arguments.insert("params".to_owned(), Value::Object(params));
    (
        CALL_OPERATION_TOOL_NAME.to_owned(),
        call_operation_arguments,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_lists_only_operations_the_agent_surface_serves() {
        let listed = the_operation_groups_served_to_agents();
        let listed_names: Vec<&str> = listed
            .groups
            .iter()
            .flat_map(|group| {
                group
                    .operations
                    .iter()
                    .map(|operation| operation.name.as_str())
            })
            .collect();
        let mut served: Vec<&str> = the_operations_served_to_agents()
            .map(|declaration| declaration.method_name)
            .collect();
        let mut listed_sorted = listed_names.clone();
        listed_sorted.sort_unstable();
        served.sort_unstable();
        assert_eq!(listed_sorted, served);
        assert!(!listed_names.contains(&"discover"));
    }

    #[test]
    fn an_operation_is_described_with_a_schema_that_resolves_its_own_references() {
        let described = the_operations_described_for(DescribeOperationsRequest {
            operations: Some(vec!["add_node".to_owned()]),
            group: None,
        })
        .unwrap_or_else(|refusal| panic!("{refusal}"));
        let [add_node] = described.operations.as_slice() else {
            panic!("one operation was asked for");
        };
        assert_eq!(add_node.name, "add_node");
        let params_schema = serde_json::to_string(&add_node.params_schema).unwrap_or_default();
        assert!(params_schema.contains("\"type\""));
        assert_eq!(add_node.params_schema["type"], "object");
        assert!(!params_schema.contains("#/components/schemas/"));
        if params_schema.contains("#/$defs/") {
            assert!(add_node.params_schema.get("$defs").is_some());
        }
    }

    #[test]
    fn the_tool_list_is_the_core_set_then_the_discovery_tools_each_with_an_object_schema() {
        let listed = the_agent_tools_listed();
        let listed_names: Vec<&str> = listed.iter().map(|tool| tool.tool_name).collect();
        assert_eq!(
            listed_names,
            [
                "describe_runtime",
                "list_streams",
                "graph",
                "transact",
                "start_playback",
                "stop_playback",
                "pause_playback",
                "resume_playback",
                "logs",
                "undo",
                LIST_OPERATION_GROUPS_TOOL_NAME,
                DESCRIBE_OPERATIONS_TOOL_NAME,
                CALL_OPERATION_TOOL_NAME,
            ]
        );
        for tool in &listed {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.tool_name);
            assert!(!tool.description.is_empty(), "{}", tool.tool_name);
        }
        let list_streams = listed
            .iter()
            .find(|tool| tool.tool_name == "list_streams")
            .unwrap_or_else(|| panic!("list_streams is listed"));
        assert_eq!(list_streams.effect, ControlOperationEffect::ReadsOnly);
        let Some(list_streams_output_schema) = &list_streams.output_schema else {
            panic!("list_streams answers one JSON object shape");
        };
        assert_eq!(list_streams_output_schema["type"], "object");
    }

    #[test]
    fn an_operation_with_no_tool_of_its_own_is_reached_through_call_operation() {
        let params: Map<String, Value> =
            serde_json::from_value(serde_json::json!({ "stream": "arm" })).unwrap_or_default();
        assert_eq!(
            the_agent_tool_call_that_reaches("graph", params.clone()),
            ("graph".to_owned(), params.clone())
        );
        let (tool_name, arguments) = the_agent_tool_call_that_reaches("add_node", params.clone());
        assert_eq!(tool_name, CALL_OPERATION_TOOL_NAME);
        assert_eq!(arguments["operation"], "add_node");
        assert_eq!(arguments["params"], Value::Object(params));
    }

    #[test]
    fn a_group_is_described_whole_and_an_unknown_operation_is_refused_by_name() {
        let described = the_operations_described_for(DescribeOperationsRequest {
            operations: None,
            group: Some(ControlOperationGroup::Dependency),
        })
        .unwrap_or_else(|refusal| panic!("{refusal}"));
        assert!(
            described
                .operations
                .iter()
                .all(|operation| operation.group == ControlOperationGroup::Dependency)
        );
        assert!(!described.operations.is_empty());
        let refusal = the_operations_described_for(DescribeOperationsRequest {
            operations: Some(vec!["expose_port".to_owned()]),
            group: None,
        })
        .unwrap_err();
        assert!(refusal.contains("`expose_port`"));
        assert!(the_operations_described_for(DescribeOperationsRequest::default()).is_err());
    }
}
