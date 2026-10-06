// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP prompts a node serves: recipes an agent follows with the tools the
//! node already serves, rendered against the live graph and the node catalog
//! at the moment one is requested.
//!
//! A prompt is text, never a mutation path — every step it lists is a call to
//! a served tool, so the tool set stays the whole of the control vocabulary.

use std::fmt::Write as _;

use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{GetPromptResult, PromptMessage, Role};
use rmcp::schemars::JsonSchema;
use rmcp::{prompt, prompt_router};
use serde::Deserialize;
use serde_json::json;
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::graph::cast_exposed_name_to_url_safe;
use streamlib::sdk::iceoryx2::{
    FRAME_HEADER_PAYLOAD_LEN_SIZE, FRAME_HEADER_SIZE, FRAME_HEADER_TIMESTAMP_NS_SIZE,
    MAX_PORT_KEY_SIZE,
};
use streamlib::sdk::json_schema::{
    GraphResponse, PortDescriptorOutput, PortInfoOutput, ProcessorDescriptorOutput,
    ProcessorNodeOutput,
};
use streamlib::sdk::processors::PROCESSOR_REGISTRY;

use crate::mcp::LocalApiMcpServerHandler;
use crate::mcp_resources::exported_live_graph_json;

/// The import path `VirtualCameraSink` registers under, which the virtual
/// camera recipe looks up in the catalog. This crate does not link the media
/// built-ins, so the wheel pins it against the built-in's own derived path.
pub const VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH: &str =
    "streamlib_media_builtins::virtual_camera_sink::VirtualCameraSink";

const LINK_ID_ARGUMENT_DESCRIPTION: &str =
    "The id of the link to splice into, as `graph` lists it under `links`.";
const TYPE_ARGUMENT_DESCRIPTION: &str = "The import path of the node class to add — a type the `streamlib://node-catalog` resource lists, or a Python class's `module:QualifiedClassName`.";
const FROM_NODE_ARGUMENT_DESCRIPTION: &str =
    "The name of the node whose output this is about, as `graph` lists it.";
const FROM_PORT_ARGUMENT_DESCRIPTION: &str =
    "The name of that node's output port, as `graph` lists it under `ports.outputs`.";
const CAMERA_NAME_ARGUMENT_DESCRIPTION: &str =
    "The camera's name in every picker. Omit for the default name.";

const INSERT_NODE_BETWEEN_LINKED_NODES_PROMPT_DESCRIPTION: &str =
    "Splice a new node into an existing link, so what the link carried passes through it.";
const FAN_OUTPUT_TO_ANOTHER_CONSUMER_PROMPT_DESCRIPTION: &str = "Add a node as one more consumer of an output port, leaving the consumers it already feeds as they are.";
const SHOW_CHANNEL_ON_VIRTUAL_CAMERA_PROMPT_DESCRIPTION: &str = "Present an output's video frames as a camera every other application on the machine can select.";
const LOOK_AT_WHAT_A_CHANNEL_CARRIES_PROMPT_DESCRIPTION: &str = "Sample one bag an output port publishes, decode it, and see the frame it names when it names one.";

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct InsertNodeBetweenLinkedNodesPromptArguments {
    #[schemars(description = LINK_ID_ARGUMENT_DESCRIPTION)]
    link_id: String,
    #[serde(rename = "type")]
    #[schemars(description = TYPE_ARGUMENT_DESCRIPTION)]
    node_type: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct FanOutputToAnotherConsumerPromptArguments {
    #[schemars(description = FROM_NODE_ARGUMENT_DESCRIPTION)]
    from_node: String,
    #[schemars(description = FROM_PORT_ARGUMENT_DESCRIPTION)]
    from_port: String,
    #[serde(rename = "type")]
    #[schemars(description = TYPE_ARGUMENT_DESCRIPTION)]
    node_type: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ShowChannelOnVirtualCameraPromptArguments {
    #[schemars(description = FROM_NODE_ARGUMENT_DESCRIPTION)]
    from_node: String,
    #[schemars(description = FROM_PORT_ARGUMENT_DESCRIPTION)]
    from_port: String,
    #[schemars(description = CAMERA_NAME_ARGUMENT_DESCRIPTION)]
    camera_name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct LookAtWhatAChannelCarriesPromptArguments {
    #[schemars(description = FROM_NODE_ARGUMENT_DESCRIPTION)]
    from_node: String,
    #[schemars(description = FROM_PORT_ARGUMENT_DESCRIPTION)]
    from_port: String,
}

#[prompt_router(vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[prompt(
        title = "Insert a node into a link",
        description = INSERT_NODE_BETWEEN_LINKED_NODES_PROMPT_DESCRIPTION
    )]
    async fn insert_node_between_linked_nodes(
        &self,
        Parameters(arguments): Parameters<InsertNodeBetweenLinkedNodesPromptArguments>,
    ) -> Result<GetPromptResult, McpError> {
        let live_graph = self.live_graph().await?;
        let recipe = insert_node_between_linked_nodes_recipe(&live_graph, &arguments)?;
        Ok(recipe.prompt_result(INSERT_NODE_BETWEEN_LINKED_NODES_PROMPT_DESCRIPTION))
    }

    #[prompt(
        title = "Fan an output to another consumer",
        description = FAN_OUTPUT_TO_ANOTHER_CONSUMER_PROMPT_DESCRIPTION
    )]
    async fn fan_output_to_another_consumer(
        &self,
        Parameters(arguments): Parameters<FanOutputToAnotherConsumerPromptArguments>,
    ) -> Result<GetPromptResult, McpError> {
        let live_graph = self.live_graph().await?;
        let recipe = fan_output_to_another_consumer_recipe(&live_graph, &arguments)?;
        Ok(recipe.prompt_result(FAN_OUTPUT_TO_ANOTHER_CONSUMER_PROMPT_DESCRIPTION))
    }

    #[prompt(
        title = "Show a channel on a virtual camera",
        description = SHOW_CHANNEL_ON_VIRTUAL_CAMERA_PROMPT_DESCRIPTION
    )]
    async fn show_channel_on_virtual_camera(
        &self,
        Parameters(arguments): Parameters<ShowChannelOnVirtualCameraPromptArguments>,
    ) -> Result<GetPromptResult, McpError> {
        let live_graph = self.live_graph().await?;
        let recipe = show_channel_on_virtual_camera_recipe(&live_graph, &arguments)?;
        Ok(recipe.prompt_result(SHOW_CHANNEL_ON_VIRTUAL_CAMERA_PROMPT_DESCRIPTION))
    }

    #[prompt(
        title = "Look at what a channel carries",
        description = LOOK_AT_WHAT_A_CHANNEL_CARRIES_PROMPT_DESCRIPTION
    )]
    async fn look_at_what_a_channel_carries(
        &self,
        Parameters(arguments): Parameters<LookAtWhatAChannelCarriesPromptArguments>,
    ) -> Result<GetPromptResult, McpError> {
        let live_graph = self.live_graph().await?;
        let recipe = look_at_what_a_channel_carries_recipe(&live_graph, &arguments)?;
        Ok(recipe.prompt_result(LOOK_AT_WHAT_A_CHANNEL_CARRIES_PROMPT_DESCRIPTION))
    }
}

impl LocalApiMcpServerHandler {
    /// The node's graph as it is now, which every recipe is rendered against.
    async fn live_graph(&self) -> Result<GraphResponse, McpError> {
        serde_json::from_value(exported_live_graph_json(&self.runtime).await?)
            .map_err(|e| McpError::internal_error(format!("graph export did not parse: {e}"), None))
    }
}

/// One numbered step of a recipe: the served tool it calls and what to pass.
struct GraphRecipeStep {
    tool_name: &'static str,
    instruction: String,
}

/// A recipe rendered against the node: what it is about, its steps, and what
/// to do when a step is refused in a known way.
struct GraphRecipe {
    introduction_text: String,
    steps: Vec<GraphRecipeStep>,
    closing_note: Option<String>,
}

impl GraphRecipe {
    /// The recipe as a `prompts/get` result: one user message carrying its text.
    fn prompt_result(&self, description: &str) -> GetPromptResult {
        GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            self.rendered_text(),
        )])
        .with_description(description)
    }

    /// The text an agent follows. Each step is its own line, `N. `tool` — …`.
    fn rendered_text(&self) -> String {
        let mut text = format!(
            "{}\n\nSteps — each calls one tool this node serves:\n",
            self.introduction_text
        );
        for (index, step) in self.steps.iter().enumerate() {
            // Writing into a `String` cannot fail.
            let _ = writeln!(
                text,
                "{}. `{}` — {}",
                index + 1,
                step.tool_name,
                step.instruction
            );
        }
        if let Some(closing_note) = &self.closing_note {
            let _ = write!(text, "\n{closing_note}\n");
        }
        text
    }
}

fn graph_recipe_step_calling_tool(
    tool_name: &'static str,
    instruction: impl Into<String>,
) -> GraphRecipeStep {
    GraphRecipeStep {
        tool_name,
        instruction: instruction.into(),
    }
}

/// The node `node_name` names once cast.
fn node_named<'graph>(
    graph: &'graph GraphResponse,
    node_name: &str,
) -> Result<&'graph ProcessorNodeOutput, McpError> {
    let cast = cast_exposed_name_to_url_safe(node_name)
        .map_err(|names_nothing| McpError::invalid_params(names_nothing.to_string(), None))?;
    graph
        .nodes
        .iter()
        .find(|node| node.name == cast)
        .ok_or_else(|| {
            McpError::invalid_params(
                format!("no node named `{node_name}` is in the graph; `graph` lists the names"),
                None,
            )
        })
}

fn input_port_of<'graph>(
    node: &'graph ProcessorNodeOutput,
    port_name: &str,
) -> Option<&'graph PortInfoOutput> {
    node.ports.inputs.iter().find(|port| port.name == port_name)
}

/// The node `from_node` names, having checked `from_port` is one of its
/// outputs.
fn node_with_output_port_named<'graph>(
    graph: &'graph GraphResponse,
    from_node: &str,
    from_port: &str,
) -> Result<&'graph ProcessorNodeOutput, McpError> {
    let node = node_named(graph, from_node)?;
    if !node.ports.outputs.iter().any(|port| port.name == from_port) {
        let output_port_names: Vec<&str> = node
            .ports
            .outputs
            .iter()
            .map(|port| port.name.as_str())
            .collect();
        return Err(McpError::invalid_params(
            format!(
                "node `{}` has no output port `{from_port}`; its outputs are {output_port_names:?}",
                node.name
            ),
            None,
        ));
    }
    Ok(node)
}

/// A registered type's input, when it has exactly one for a recipe to wire.
fn sole_input_port(entry: &ProcessorDescriptorOutput) -> Option<&PortDescriptorOutput> {
    match entry.inputs.as_slice() {
        [sole_input] => Some(sole_input),
        _ => None,
    }
}

/// The catalog entry for one import path, or `None` when nothing is
/// registered under it — a string that is not an import path included.
fn catalog_entry_for(processor_type: &str) -> Option<ProcessorDescriptorOutput> {
    let processor_class_import_path = ProcessorClassImportPath::new(processor_type).ok()?;
    PROCESSOR_REGISTRY
        .descriptor(&processor_class_import_path)
        .map(|descriptor| ProcessorDescriptorOutput::from(&descriptor))
}

fn catalog_entry_json_block(entry: &ProcessorDescriptorOutput) -> Result<String, McpError> {
    let text = serde_json::to_string_pretty(entry).map_err(|e| {
        McpError::internal_error(format!("catalog entry rendering failed: {e}"), None)
    })?;
    Ok(format!("```json\n{text}\n```"))
}

/// What the catalog says about a type an agent is about to add, or why it says
/// nothing yet.
fn catalog_introduction_for(
    node_type: &str,
    entry: Option<&ProcessorDescriptorOutput>,
) -> Result<String, McpError> {
    match entry {
        Some(entry) => Ok(format!(
            "This node's catalog entry for `{node_type}`, read now — `config_schema` is what \
             `add_node`'s `config` takes, and `inputs` and `outputs` are its ports:\n{}",
            catalog_entry_json_block(entry)?
        )),
        None => Ok(format!(
            "`{node_type}` is not in this node's catalog yet. A Python class enters it when its \
             module is imported, which `add_node` does; its ports then show in `graph`, and its \
             config takes the keys its `__init__`'s config class declares."
        )),
    }
}

fn add_node_step(node_type: &str) -> GraphRecipeStep {
    graph_recipe_step_calling_tool(
        "add_node",
        format!(
            "`type`: `{node_type}`; `config`: an object of the keys its config schema declares, \
             or omit it when there are none. Keep the `name` it returns."
        ),
    )
}

fn find_the_added_node_step(port_directions: &str) -> GraphRecipeStep {
    graph_recipe_step_calling_tool(
        "graph",
        format!("find the node with that `name`; {port_directions} name the ports to wire."),
    )
}

fn insert_node_between_linked_nodes_recipe(
    graph: &GraphResponse,
    arguments: &InsertNodeBetweenLinkedNodesPromptArguments,
) -> Result<GraphRecipe, McpError> {
    let link_id = arguments.link_id.as_str();
    let node_type = arguments.node_type.as_str();
    let link = graph
        .links
        .iter()
        .find(|link| link.id == link_id)
        .ok_or_else(|| {
            McpError::invalid_params(
                format!("no link with id `{link_id}` is in the graph; `graph` lists the links"),
                None,
            )
        })?;
    let source = node_named(graph, &link.source.node)?;
    let target = node_named(graph, &link.target.node)?;
    let source_port = link.source.port.as_str();
    let target_port = link.target.port.as_str();
    let source_name = source.name.as_str();
    let target_name = target.name.as_str();

    let inserted_type_entry = catalog_entry_for(node_type);
    let target_port_takes_one_inbound_link =
        input_port_of(target, target_port).is_some_and(|port| port.audio_window.is_some());

    let connect_source_to_inserted = graph_recipe_step_calling_tool(
        "connect",
        format!(
            "`from_node`: `{source_name}`, `from_port`: `{source_port}`, `to_node`: the new \
             node's `name`, `to_port`: its input port."
        ),
    );
    let connect_inserted_to_target = graph_recipe_step_calling_tool(
        "connect",
        format!(
            "`from_node`: the new node's `name`, `from_port`: its output port, `to_node`: \
             `{target_name}`, `to_port`: `{target_port}`."
        ),
    );
    let disconnect_the_replaced_link =
        graph_recipe_step_calling_tool("disconnect", format!("`link_id`: `{link_id}`."));
    let wiring_steps = if target_port_takes_one_inbound_link {
        [
            disconnect_the_replaced_link,
            connect_source_to_inserted,
            connect_inserted_to_target,
        ]
    } else {
        [
            connect_source_to_inserted,
            connect_inserted_to_target,
            disconnect_the_replaced_link,
        ]
    };
    let mut steps = vec![
        add_node_step(node_type),
        find_the_added_node_step("its `ports.inputs` and `ports.outputs`"),
    ];
    steps.extend(wiring_steps);
    steps.push(graph_recipe_step_calling_tool(
        "graph",
        format!(
            "confirm the links both `connect` calls returned have `state` `wired`, the new node's \
             `components.state` is `Running`, and link `{link_id}` is gone. A Python node runs in \
             a helper process, so each link onto it reads `pending` until that helper has opened \
             its port — read `graph` again. A link that reads `error` carries the helper's own \
             reason in `error_reason` and will never carry a bag: `disconnect` it and fix what \
             the reason names."
        ),
    ));

    let closing_note = target_port_takes_one_inbound_link.then(|| {
        format!(
            "The link goes before the new node is wired because `{target_name}` port \
             `{target_port}` declares an audio window contract, so it takes a single inbound \
             link; `{target_name}` receives nothing between the `disconnect` and the second \
             `connect`. If a `connect` after the `disconnect` is refused, `connect` \
             `{source_name}` port `{source_port}` to `{target_name}` port `{target_port}` again \
             to restore what the link carried."
        )
    });

    Ok(GraphRecipe {
        introduction_text: format!(
            "Insert a `{node_type}` node into link `{link_id}`, which carries `{source_name}` \
             port `{source_port}` to `{target_name}` port `{target_port}`.\n\n{}",
            catalog_introduction_for(node_type, inserted_type_entry.as_ref())?
        ),
        steps,
        closing_note,
    })
}

fn fan_output_to_another_consumer_recipe(
    graph: &GraphResponse,
    arguments: &FanOutputToAnotherConsumerPromptArguments,
) -> Result<GraphRecipe, McpError> {
    let source = node_with_output_port_named(graph, &arguments.from_node, &arguments.from_port)?;
    let from_port = arguments.from_port.as_str();
    let node_type = arguments.node_type.as_str();
    let source_name = source.name.as_str();
    let consumer_type_entry = catalog_entry_for(node_type);

    Ok(GraphRecipe {
        introduction_text: format!(
            "Add a `{node_type}` node as another consumer of `{source_name}` port `{from_port}`. \
             The links that port already feeds stay as they are.\n\n{}",
            catalog_introduction_for(node_type, consumer_type_entry.as_ref())?
        ),
        steps: vec![
            add_node_step(node_type),
            find_the_added_node_step("its `ports.inputs`"),
            graph_recipe_step_calling_tool(
                "connect",
                format!(
                    "`from_node`: `{source_name}`, `from_port`: `{from_port}`, `to_node`: the new \
                     node's `name`, `to_port`: its input port."
                ),
            ),
            graph_recipe_step_calling_tool(
                "graph",
                format!(
                    "confirm the link `connect` returned has `state` `wired`, the new node's \
                     `components.state` is `Running`, and `{source_name}`'s other links are still \
                     there. A Python node runs in a helper process, so the link reads `pending` \
                     until that helper has opened its port — read `graph` again. A link that \
                     reads `error` carries the helper's own reason in `error_reason` and will \
                     never carry a bag: `disconnect` it and fix what the reason names."
                ),
            ),
        ],
        closing_note: None,
    })
}

fn show_channel_on_virtual_camera_recipe(
    graph: &GraphResponse,
    arguments: &ShowChannelOnVirtualCameraPromptArguments,
) -> Result<GraphRecipe, McpError> {
    let source = node_with_output_port_named(graph, &arguments.from_node, &arguments.from_port)?;
    let from_port = arguments.from_port.as_str();
    let camera_name = arguments.camera_name.as_deref();
    let virtual_camera_sink = catalog_entry_for(VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH)
        .ok_or_else(|| {
            McpError::invalid_params(format!(
                "this node's catalog has no `{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}`: \
                 the virtual camera is a Linux built-in"
            ), None)
        })?;
    let video_input = sole_input_port(&virtual_camera_sink).ok_or_else(|| {
        McpError::internal_error(
            format!(
                "`{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}` is registered with {} input \
             ports rather than one",
                virtual_camera_sink.inputs.len()
            ),
            None,
        )
    })?;
    let video_input_port = video_input.name.as_str();
    let config_instruction = match camera_name {
        Some(camera_name) => format!("`config`: `{}`", json!({ "name": camera_name })),
        None => "`config`: `{}`, which takes the default camera name".to_string(),
    };
    let source_name = source.name.as_str();

    Ok(GraphRecipe {
        introduction_text: format!(
            "Present `{source_name}` port `{from_port}` as a virtual camera: one camera every \
             other application on this machine can select, there while its node runs and gone \
             when it is removed.\n\nThis node's catalog entry for it, read now:\n{}",
            catalog_entry_json_block(&virtual_camera_sink)?
        ),
        steps: vec![
            graph_recipe_step_calling_tool(
                "add_node",
                format!(
                    "`type`: `{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}`; \
                     {config_instruction}. Keep the `name` it returns."
                ),
            ),
            graph_recipe_step_calling_tool(
                "connect",
                format!(
                    "`from_node`: `{source_name}`, `from_port`: `{from_port}`, `to_node`: that \
                     `name`, `to_port`: `{video_input_port}`."
                ),
            ),
            graph_recipe_step_calling_tool(
                "graph",
                "confirm the link `connect` returned has `state` `wired` and the camera's node's \
                 `components.state` is `Running`. `VirtualCameraSink` is a native built-in, so \
                 its link is wired as soon as `connect` returns; a link onto a node in a helper \
                 process instead reads `pending` until that helper answers, and `error` with the \
                 helper's own reason in `error_reason` where it could not open its port.",
            ),
        ],
        closing_note: Some(
            "The camera goes away with its node: `remove_node` with that `name`.".to_string(),
        ),
    })
}

fn look_at_what_a_channel_carries_recipe(
    graph: &GraphResponse,
    arguments: &LookAtWhatAChannelCarriesPromptArguments,
) -> Result<GraphRecipe, McpError> {
    let source = node_with_output_port_named(graph, &arguments.from_node, &arguments.from_port)?;
    let from_port = arguments.from_port.as_str();
    let channel = format!("{}/{}/{from_port}", graph.runtime_name, source.name);

    Ok(GraphRecipe {
        introduction_text: format!(
            "Look at what `{}` publishes on port `{from_port}`, the channel `{channel}`.",
            source.name
        ),
        steps: vec![
            graph_recipe_step_calling_tool(
                "tap",
                format!(
                    "`channel`: `{channel}`, `count`: 1. A bag's `hex_preview` is the channel's \
                     wire bytes: a {FRAME_HEADER_SIZE}-byte frame header — a \
                     {MAX_PORT_KEY_SIZE}-byte port key, the producer's timestamp as \
                     {FRAME_HEADER_TIMESTAMP_NS_SIZE} little-endian bytes of monotonic \
                     nanoseconds, then the payload length as {FRAME_HEADER_PAYLOAD_LEN_SIZE} \
                     little-endian bytes — followed by that many bytes of one msgpack map, the \
                     bag. `received` of 0 means nothing was published inside the sample window; \
                     tap again."
                ),
            ),
            graph_recipe_step_calling_tool(
                "exchange",
                "`surface_id`: the bag's `surface_id`, when it carries one. The result is that \
                 frame's picture. A refusal naming a recycled frame means the frame was retired \
                 first; tap a newer bag and exchange that.",
            ),
        ],
        closing_note: Some(
            "A bag naming no `surface_id` has nothing to exchange: its keys are what the channel \
             carries."
                .to_string(),
        ),
    })
}
