// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The agent surface: the operations it lists as tools of their own, and the three discovery
//! tools that reach every other operation it serves.

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ProjectActionRefusalOnTheWire,
};

use crate::control_operation::{
    ControlOperationDeclaration, ControlOperationEffect, ControlOperationExample,
    ControlOperationGroup, ControlOperationStability,
};
use crate::control_operation_refusals::the_refusal_of_an_unknown_operation;
use crate::control_operations::CONTROL_OPERATIONS;

/// The name of the discovery tool that lists the groups and the operations in each.
pub const LIST_OPERATION_GROUPS_TOOL_NAME: &str = "list_operation_groups";

/// The name of the discovery tool that describes operations with their schemas.
pub const DESCRIBE_OPERATIONS_TOOL_NAME: &str = "describe_operations";

/// The name of the discovery tool that calls any operation the agent surface serves.
pub const CALL_OPERATION_TOOL_NAME: &str = "call_operation";

/// The guidance `server/discover` hands an agent before its first call: how the runtime is laid
/// out, the three loops by the tools each uses, then where everything else is found. Every name
/// in backticks is an operation the agent surface serves, or one of its discovery tools.
pub const LOCAL_API_MCP_SERVER_INSTRUCTIONS: &str = concat!(
    "This is the control plane of this machine's one StreamLib runtime.\n",
    "A project is a folder on this machine, named by its absolute path as the project directory; it holds streams, its node code and its assets.\n",
    "A stream is a graph of nodes and the links between their ports, named by the stream id `list_streams` answers; a node is named by the UUID `graph` lists.\n",
    "A project plays and stops as a whole, and its graphs read and edit whether it plays or not.\n",
    "Every change is one transaction by its author on the project's history, applied whole or refused whole in the runtime's own words, and `undo` takes the author's latest back.\n",
    "\n",
    "Reading: `list_streams` lists every stream with its project; `graph` answers a stream's nodes, ports, links and each node's state; `list_node_types` lists the node types a project offers and `describe_node_types` answers their typed inputs and outputs; `logs` reads what the project's streams wrote.\n",
    "Building a graph: `check_link_candidates` finds the inputs an output fits, `check_link` answers what a link would do without making it, `transact` adds nodes, sets values and makes links as one edit, and `graph` confirms each link: pending until a node in a helper process opens its port, then wired, or reading error with the reason in error_reason. Both ends of a link are ports in the one stream it names; a stream reads another stream's port at a MoQ relay through a MoqTrackReceive node.\n",
    "Writing a Python node: `create_node_file` starts one from the template, `read_project_file` and `write_project_file` read and write it, `check_project_code` checks it without running a stream and says what to fix and where, and `transact` places it.\n",
    "Writing a control or a node body: `create_node_ui_element` starts one from the extension SDK kit's template and registers it, then the same file tools and `check_project_code`.\n",
    "Playback: `start_playback` and `stop_playback`.\n",
    "\n",
    "Everything else, such as `list_pin_types`, the project's assets, files, imports and views, and every single graph edit: `list_operation_groups` lists every operation by group, `describe_operations` states one's params and result with a worked example, and `call_operation` calls it by name.",
);

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
    /// A worked example of a call; absent on an operation another replaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<AgentOperationExample>,
}

/// A worked example of calling one operation.
#[derive(Debug, PartialEq, Serialize, JsonSchema)]
pub struct AgentOperationExample {
    /// The `params` a call sends.
    pub params: Value,
    /// What that call answers; given for an operation the three agent loops call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<Value>,
    /// A refusal of a call of it, in the one refusal shape; given for an operation the three
    /// agent loops call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Value>,
}

impl AgentOperationExample {
    /// `example` as the values it renders.
    pub fn of(example: &ControlOperationExample) -> Self {
        Self {
            params: (example.params)(),
            answer: example.answer.map(|answer| answer()),
            refusal: example.refusal.map(|refusal| refusal()),
        }
    }
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
        agent_usage_note: "List every operation this runtime serves, by group, each with its name and one sentence saying what it does. The tools in this tool list are the ones used most; every other operation — `list_pin_types`, a project's assets, its other files, imports and views, each single graph edit, `redo`, viewers, pausing and resuming playback, `describe_runtime` — is reached through `call_operation`. Start here when no listed tool does what you need, then read the operation's params with `describe_operations`.",
        effect: ControlOperationEffect::ReadsOnly,
        request_json_schema: the_json_schema_of::<ListOperationGroupsRequest>,
        result_json_schema: Some(the_json_schema_of::<ListOperationGroupsResult>),
    },
    AgentDiscoveryToolDeclaration {
        tool_name: DESCRIBE_OPERATIONS_TOOL_NAME,
        one_line_doc: "Describe operations: what each does, the params it takes and what it answers.",
        agent_usage_note: "Describe operations by name, or every operation of one group: what each reads, changes and answers, the JSON Schema of its params and of its result, whether it only reads, the contract version it arrived in, and an `example` of the params a call sends; an operation of the three loops (building a graph, writing a Python node, writing a control) also shows what that call answers and a refusal of it. Call it before the first `call_operation` of an operation, and send the params exactly as its `params_schema` states them.",
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
    for declaration in the_operations_served_to_agents()
        .filter(|declaration| declaration.is_listed_by_the_discovery_tools())
    {
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

/// What `describe_operations` answers for `request`, or the refusal saying why it describes
/// nothing.
pub fn the_operations_described_for(
    DescribeOperationsRequest { operations, group }: DescribeOperationsRequest,
) -> Result<DescribeOperationsResult, Box<ProjectActionRefusalOnTheWire>> {
    let described: Vec<&ControlOperationDeclaration> = match (operations, group) {
        (Some(names), None) => {
            if let Some(unknown) = names
                .iter()
                .find(|name| the_operation_served_to_agents_as(name).is_none())
            {
                return Err(Box::new(the_refusal_of_an_unknown_operation(
                    Some(DESCRIBE_OPERATIONS_TOOL_NAME),
                    unknown,
                )));
            }
            the_operations_served_to_agents()
                .filter(|declaration| names.iter().any(|name| name == declaration.method_name))
                .collect()
        }
        (None, Some(group)) => the_operations_served_to_agents()
            .filter(|declaration| {
                declaration.group == group && declaration.is_listed_by_the_discovery_tools()
            })
            .collect(),
        _ => {
            return Err(Box::new(ProjectActionRefusalOnTheWire::of_the_action(
                DESCRIBE_OPERATIONS_TOOL_NAME,
                ControlOperationRefusalCode::Refused,
                format!(
                    "{DESCRIBE_OPERATIONS_TOOL_NAME} failed: send exactly one of `operations` \
                     and `group`"
                ),
            )));
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
                example: declaration.example.as_ref().map(AgentOperationExample::of),
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
    fn every_group_lists_only_operations_the_agent_surface_serves_and_nothing_replaced() {
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
            .filter(|declaration| declaration.replaced_by.is_none())
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
        .unwrap_or_else(|refusal| panic!("{}", refusal.runtime_words));
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
    fn an_operation_is_described_with_its_worked_example_and_a_loop_operation_with_its_answer_and_refusal()
     {
        let described = the_operations_described_for(DescribeOperationsRequest {
            operations: Some(vec![
                "add_node".to_owned(),
                "check_link".to_owned(),
                "set_node_config".to_owned(),
            ]),
            group: None,
        })
        .unwrap_or_else(|refusal| panic!("{}", refusal.runtime_words));
        let [add_node, check_link, set_node_config] = described.operations.as_slice() else {
            panic!("three operations were asked for");
        };
        let Some(add_node_example) = &add_node.example else {
            panic!("add_node shows an example");
        };
        assert!(add_node_example.params["stream"].is_string());
        assert_eq!(
            (&add_node_example.answer, &add_node_example.refusal),
            (&None, &None)
        );
        let Some(check_link_example) = &check_link.example else {
            panic!("check_link shows an example");
        };
        assert!(
            check_link_example
                .answer
                .as_ref()
                .is_some_and(|answer| answer["outcome"] == "refused")
        );
        assert!(
            check_link_example
                .refusal
                .as_ref()
                .is_some_and(|refusal| refusal["refusal_code"].is_string())
        );
        assert_eq!(set_node_config.example, None);
    }

    #[test]
    fn the_tool_list_is_the_core_set_then_the_discovery_tools_each_with_an_object_schema() {
        let listed = the_agent_tools_listed();
        let listed_names: Vec<&str> = listed.iter().map(|tool| tool.tool_name).collect();
        assert_eq!(
            listed_names,
            [
                "list_streams",
                "graph",
                "transact",
                "check_link",
                "check_link_candidates",
                "start_playback",
                "stop_playback",
                "read_project_file",
                "create_node_file",
                "create_node_ui_element",
                "write_project_file",
                "check_project_code",
                "list_node_types",
                "describe_node_types",
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

    /// The most characters the instructions may hold: short enough to read before a first call.
    const INSTRUCTIONS_LENGTH_BOUND: usize = 3_000;

    #[test]
    fn every_name_in_backticks_in_the_instructions_is_served_to_agents_and_the_text_is_short() {
        let served: Vec<&str> = the_operations_served_to_agents()
            .filter(|declaration| declaration.replaced_by.is_none())
            .map(|declaration| declaration.method_name)
            .chain(AGENT_DISCOVERY_TOOLS.iter().map(|tool| tool.tool_name))
            .collect();
        let named: Vec<&str> = LOCAL_API_MCP_SERVER_INSTRUCTIONS
            .split('`')
            .skip(1)
            .step_by(2)
            .collect();
        assert!(named.contains(&"list_pin_types"), "{named:?}");
        for name in &named {
            assert!(
                served.contains(name),
                "the instructions name `{name}`, which agents are not served"
            );
        }
        assert_eq!(
            LOCAL_API_MCP_SERVER_INSTRUCTIONS.matches('`').count() % 2,
            0
        );
        assert!(
            LOCAL_API_MCP_SERVER_INSTRUCTIONS.chars().count() <= INSTRUCTIONS_LENGTH_BOUND,
            "the instructions hold {} characters",
            LOCAL_API_MCP_SERVER_INSTRUCTIONS.chars().count()
        );
        for tool in the_agent_tools_listed() {
            assert!(
                named.contains(&tool.tool_name),
                "the instructions do not name the listed tool `{}`",
                tool.tool_name
            );
        }
    }

    #[test]
    fn the_tool_list_holds_nineteen_tools_and_no_replaced_operation() {
        let listed = the_agent_tools_listed();
        assert_eq!(listed.len(), 19);
        for tool in &listed {
            assert!(
                the_operation_served_to_agents_as(tool.tool_name)
                    .is_none_or(|declaration| declaration.replaced_by.is_none()),
                "{} is replaced",
                tool.tool_name
            );
        }
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
        .unwrap_or_else(|refusal| panic!("{}", refusal.runtime_words));
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
        assert!(refusal.runtime_words.contains("`expose_port`"));
        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::OperationNotFound
        );
        assert!(the_operations_described_for(DescribeOperationsRequest::default()).is_err());
    }

    #[test]
    fn a_replaced_operation_is_not_listed_and_is_still_described_and_reached_by_name() {
        let replaced = ["set_node_config", "set_pin_value"];
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
        assert!(
            listed_names.contains(&"set_node_input_value"),
            "{listed_names:?}"
        );
        for replaced in replaced {
            assert!(!listed_names.contains(&replaced), "{replaced} is listed");
            let described = the_operations_described_for(DescribeOperationsRequest {
                operations: Some(vec![replaced.to_owned()]),
                group: None,
            })
            .unwrap_or_else(|refusal| panic!("{}", refusal.runtime_words));
            assert_eq!(described.operations.len(), 1);
            assert!(
                described.operations[0]
                    .usage
                    .contains("set_node_input_value"),
                "{replaced} names its replacement"
            );
        }
        let described_graph_group = the_operations_described_for(DescribeOperationsRequest {
            operations: None,
            group: Some(ControlOperationGroup::Graph),
        })
        .unwrap_or_else(|refusal| panic!("{}", refusal.runtime_words));
        assert!(
            described_graph_group
                .operations
                .iter()
                .all(|operation| !replaced.contains(&operation.name.as_str()))
        );
    }
}
