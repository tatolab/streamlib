// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP prompts a node serves: recipes an agent follows with the tools the
//! node already serves, rendered against the live graph and the processor
//! catalog at the moment one is requested.
//!
//! A prompt is text, never a mutation path — every step it lists is a call to
//! a served tool, so the tool set stays the whole of the control vocabulary.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::iceoryx2::{FRAME_HEADER_SIZE, source_channel_name};
use streamlib::sdk::json_schema::{
    GraphResponse, LinkOutput, ProcessorDescriptorOutput, ProcessorNodeOutput,
};
use streamlib::sdk::runtime::RuntimeOperations;

use crate::handlers::processor_catalog_of_this_process;
use crate::mcp::RpcError;

/// The import path `VirtualCameraSink` registers under, which the virtual
/// camera recipe looks up in the catalog. This crate does not link the media
/// built-ins, so the wheel pins it against the built-in's own derived path.
pub const VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH: &str =
    "streamlib_media_builtins::virtual_camera_sink::VirtualCameraSink";

const INSERT_PROCESSOR_BETWEEN_LINKED_PROCESSORS_PROMPT_NAME: &str =
    "insert_processor_between_linked_processors";
const FAN_OUTPUT_TO_ANOTHER_CONSUMER_PROMPT_NAME: &str = "fan_output_to_another_consumer";
const SHOW_CHANNEL_ON_VIRTUAL_CAMERA_PROMPT_NAME: &str = "show_channel_on_virtual_camera";
const LOOK_AT_WHAT_A_CHANNEL_CARRIES_PROMPT_NAME: &str = "look_at_what_a_channel_carries";

// The frame header's two fixed fields after the port key: the producer's
// timestamp (`i64`) and the payload length (`u32`), both little-endian.
const FRAME_HEADER_TIMESTAMP_BYTES: usize = 8;
const FRAME_HEADER_PAYLOAD_LENGTH_BYTES: usize = 4;

struct GraphRecipePromptArgument {
    name: &'static str,
    description: &'static str,
    required: bool,
}

type GraphRecipeRendering =
    fn(&GraphResponse, &GraphRecipePromptArguments) -> std::result::Result<GraphRecipe, RpcError>;

struct GraphRecipePromptDefinition {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    arguments: &'static [GraphRecipePromptArgument],
    render_recipe_against_live_graph: GraphRecipeRendering,
}

const PROCESSOR_TYPE_ARGUMENT: GraphRecipePromptArgument = GraphRecipePromptArgument {
    name: "processor_type",
    description: "The import path of the processor class to add — a type the `streamlib://processor-catalog` resource lists, or a Python class's `module:QualifiedClassName`.",
    required: true,
};
const FROM_PROCESSOR_ID_ARGUMENT: GraphRecipePromptArgument = GraphRecipePromptArgument {
    name: "from_processor_id",
    description: "The id of the processor whose output this is about, as `graph` reports it.",
    required: true,
};
const FROM_PORT_ARGUMENT: GraphRecipePromptArgument = GraphRecipePromptArgument {
    name: "from_port",
    description: "The name of that processor's output port, as `graph` lists it under `ports.outputs`.",
    required: true,
};

const GRAPH_RECIPE_PROMPT_DEFINITIONS: &[GraphRecipePromptDefinition] = &[
    GraphRecipePromptDefinition {
        name: INSERT_PROCESSOR_BETWEEN_LINKED_PROCESSORS_PROMPT_NAME,
        title: "Insert a processor into a link",
        description: "Splice a new processor into an existing link, so what the link carried passes through it.",
        arguments: &[
            GraphRecipePromptArgument {
                name: "link_id",
                description: "The id of the link to splice into, as `graph` lists it under `links`.",
                required: true,
            },
            PROCESSOR_TYPE_ARGUMENT,
        ],
        render_recipe_against_live_graph: insert_processor_between_linked_processors_recipe,
    },
    GraphRecipePromptDefinition {
        name: FAN_OUTPUT_TO_ANOTHER_CONSUMER_PROMPT_NAME,
        title: "Fan an output to another consumer",
        description: "Add a processor as one more consumer of an output port, leaving the consumers it already feeds as they are.",
        arguments: &[
            FROM_PROCESSOR_ID_ARGUMENT,
            FROM_PORT_ARGUMENT,
            PROCESSOR_TYPE_ARGUMENT,
        ],
        render_recipe_against_live_graph: fan_output_to_another_consumer_recipe,
    },
    GraphRecipePromptDefinition {
        name: SHOW_CHANNEL_ON_VIRTUAL_CAMERA_PROMPT_NAME,
        title: "Show a channel on a virtual camera",
        description: "Present an output's video frames as a camera every other application on the machine can select.",
        arguments: &[
            FROM_PROCESSOR_ID_ARGUMENT,
            FROM_PORT_ARGUMENT,
            GraphRecipePromptArgument {
                name: "camera_name",
                description: "The camera's name in every picker. Omit for the default name.",
                required: false,
            },
        ],
        render_recipe_against_live_graph: show_channel_on_virtual_camera_recipe,
    },
    GraphRecipePromptDefinition {
        name: LOOK_AT_WHAT_A_CHANNEL_CARRIES_PROMPT_NAME,
        title: "Look at what a channel carries",
        description: "Sample one bag an output port publishes, decode it, and see the frame it names when it names one.",
        arguments: &[FROM_PROCESSOR_ID_ARGUMENT, FROM_PORT_ARGUMENT],
        render_recipe_against_live_graph: look_at_what_a_channel_carries_recipe,
    },
];

/// The `prompts/list` result.
pub(crate) fn prompts_list_result() -> Value {
    let prompts: Vec<Value> = GRAPH_RECIPE_PROMPT_DEFINITIONS
        .iter()
        .map(|definition| {
            let arguments: Vec<Value> = definition
                .arguments
                .iter()
                .map(|argument| {
                    json!({
                        "name": argument.name,
                        "description": argument.description,
                        "required": argument.required,
                    })
                })
                .collect();
            json!({
                "name": definition.name,
                "title": definition.title,
                "description": definition.description,
                "arguments": arguments,
            })
        })
        .collect();
    json!({ "prompts": prompts })
}

/// Answer `prompts/get`, rendering the named recipe against the node as it is
/// now.
pub(crate) async fn get_prompt(
    runtime: &Arc<dyn RuntimeOperations>,
    params: Value,
) -> std::result::Result<Value, RpcError> {
    #[derive(Deserialize)]
    struct GetPromptParams {
        name: String,
        #[serde(default)]
        arguments: Map<String, Value>,
    }
    let GetPromptParams { name, arguments } = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("malformed prompts/get params: {e}")))?;
    let definition = GRAPH_RECIPE_PROMPT_DEFINITIONS
        .iter()
        .find(|definition| definition.name == name)
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "no prompt named `{name}`; `prompts/list` names the ones this node serves"
            ))
        })?;
    let prompt_arguments = GraphRecipePromptArguments {
        prompt_name: definition.name,
        arguments,
    };

    let recipe = (definition.render_recipe_against_live_graph)(
        &live_graph(runtime).await?,
        &prompt_arguments,
    )?;

    Ok(json!({
        "description": definition.description,
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": recipe.rendered_text() },
        }],
    }))
}

/// One numbered step of a recipe: the served tool it calls and what to pass.
pub(crate) struct GraphRecipeStep {
    pub(crate) tool_name: &'static str,
    instruction: String,
}

/// A recipe rendered against the node: what it is about, its steps, and what
/// to do when a step is refused in a known way.
pub(crate) struct GraphRecipe {
    context: String,
    pub(crate) steps: Vec<GraphRecipeStep>,
    closing_note: Option<String>,
}

impl GraphRecipe {
    /// The text an agent follows. Each step is its own line, `N. `tool` — …`.
    fn rendered_text(&self) -> String {
        let mut text = format!(
            "{}\n\nSteps — each calls one tool this node serves:\n",
            self.context
        );
        for (index, step) in self.steps.iter().enumerate() {
            text.push_str(&format!(
                "{}. `{}` — {}\n",
                index + 1,
                step.tool_name,
                step.instruction
            ));
        }
        if let Some(closing_note) = &self.closing_note {
            text.push('\n');
            text.push_str(closing_note);
            text.push('\n');
        }
        text
    }
}

fn step(tool_name: &'static str, instruction: impl Into<String>) -> GraphRecipeStep {
    GraphRecipeStep {
        tool_name,
        instruction: instruction.into(),
    }
}

/// A prompt's string arguments, refused by name when a required one is absent.
struct GraphRecipePromptArguments {
    prompt_name: &'static str,
    arguments: Map<String, Value>,
}

impl GraphRecipePromptArguments {
    fn required(&self, argument_name: &str) -> std::result::Result<&str, RpcError> {
        self.optional(argument_name)?.ok_or_else(|| {
            RpcError::invalid_params(format!(
                "prompt `{}` needs the `{argument_name}` argument",
                self.prompt_name
            ))
        })
    }

    fn optional(&self, argument_name: &str) -> std::result::Result<Option<&str>, RpcError> {
        match self.arguments.get(argument_name) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.as_str())),
            Some(other) => Err(RpcError::invalid_params(format!(
                "prompt `{}` argument `{argument_name}` must be a string, got {other}",
                self.prompt_name
            ))),
        }
    }
}

async fn live_graph(
    runtime: &Arc<dyn RuntimeOperations>,
) -> std::result::Result<GraphResponse, RpcError> {
    let graph = runtime
        .to_json_async()
        .await
        .map_err(|e| RpcError::internal(format!("graph export failed: {e}")))?;
    serde_json::from_value(graph)
        .map_err(|e| RpcError::internal(format!("graph export did not parse: {e}")))
}

fn node_with_id<'graph>(
    graph: &'graph GraphResponse,
    processor_id: &str,
) -> std::result::Result<&'graph ProcessorNodeOutput, RpcError> {
    graph
        .nodes
        .iter()
        .find(|node| node.id == processor_id)
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "no processor with id `{processor_id}` is in the graph; `graph` lists the ids"
            ))
        })
}

/// The processor `from_processor_id` names, having checked `from_port` is one
/// of its outputs.
fn output_port_named_by_arguments<'graph>(
    graph: &'graph GraphResponse,
    prompt_arguments: &GraphRecipePromptArguments,
) -> std::result::Result<(&'graph ProcessorNodeOutput, String), RpcError> {
    let node = node_with_id(graph, prompt_arguments.required("from_processor_id")?)?;
    let from_port = prompt_arguments.required("from_port")?;
    if !node.ports.outputs.iter().any(|port| port.name == from_port) {
        let output_port_names: Vec<&str> = node
            .ports
            .outputs
            .iter()
            .map(|port| port.name.as_str())
            .collect();
        return Err(RpcError::invalid_params(format!(
            "processor `{}` has no output port `{from_port}`; its outputs are {output_port_names:?}",
            node.id
        )));
    }
    Ok((node, from_port.to_string()))
}

fn node_label(node: &ProcessorNodeOutput) -> String {
    format!("`{}` (id `{}`)", node.display_name, node.id)
}

fn catalog_entry_for(processor_type: &str) -> Option<ProcessorDescriptorOutput> {
    processor_catalog_of_this_process()
        .processors
        .into_iter()
        .find(|entry| entry.processor_class_import_path.as_str() == processor_type)
}

fn pretty_json_block(value: &impl serde::Serialize) -> std::result::Result<String, RpcError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| RpcError::internal(format!("catalog entry rendering failed: {e}")))?;
    Ok(format!("```json\n{text}\n```"))
}

/// What the catalog says about a type an agent is about to add, or why it says
/// nothing yet.
fn catalog_context_for(processor_type: &str) -> std::result::Result<String, RpcError> {
    match catalog_entry_for(processor_type) {
        Some(entry) => Ok(format!(
            "This node's catalog entry for `{processor_type}`, read now — `config_schema` is \
             what `add_processor`'s `config` takes, and `inputs` and `outputs` are its ports:\n{}",
            pretty_json_block(&entry)?
        )),
        None => Ok(format!(
            "`{processor_type}` is not in this node's catalog yet. A Python class enters it when \
             its module is imported, which `add_processor` does; its ports then show in `graph`, \
             and its config takes the keys its `__init__`'s config class declares."
        )),
    }
}

fn add_processor_step(processor_type: &str) -> GraphRecipeStep {
    step(
        "add_processor",
        format!(
            "`type`: `{processor_type}`; `config`: an object of the keys its config schema \
             declares, or omit it when there are none. Keep the `processor_id` it returns."
        ),
    )
}

fn find_the_added_node_step(port_directions: &str) -> GraphRecipeStep {
    step(
        "graph",
        format!(
            "find the node whose `id` is that `processor_id`; {port_directions} name the ports to \
             wire."
        ),
    )
}

fn insert_processor_between_linked_processors_recipe(
    graph: &GraphResponse,
    prompt_arguments: &GraphRecipePromptArguments,
) -> std::result::Result<GraphRecipe, RpcError> {
    let link_id = prompt_arguments.required("link_id")?;
    let processor_type = prompt_arguments.required("processor_type")?;
    let link: &LinkOutput = graph
        .links
        .iter()
        .find(|link| link.id == link_id)
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "no link with id `{link_id}` is in the graph; `graph` lists the links"
            ))
        })?;
    let source = node_with_id(graph, &link.source.processor_id)?;
    let target = node_with_id(graph, &link.target.processor_id)?;
    let source_port = &link.source.port_name;
    let target_port = &link.target.port_name;

    Ok(GraphRecipe {
        context: format!(
            "Insert a `{processor_type}` processor into link `{link_id}`, which carries {} port \
             `{source_port}` to {} port `{target_port}`.\n\n{}",
            node_label(source),
            node_label(target),
            catalog_context_for(processor_type)?
        ),
        steps: vec![
            add_processor_step(processor_type),
            find_the_added_node_step("its `ports.inputs` and `ports.outputs`"),
            step(
                "connect",
                format!(
                    "`from_processor_id`: `{}`, `from_port`: `{source_port}`, \
                     `to_processor_id`: the new `processor_id`, `to_port`: its input port.",
                    source.id
                ),
            ),
            step(
                "connect",
                format!(
                    "`from_processor_id`: the new `processor_id`, `from_port`: its output port, \
                     `to_processor_id`: `{}`, `to_port`: `{target_port}`.",
                    target.id
                ),
            ),
            step("disconnect", format!("`link_id`: `{link_id}`.")),
            step(
                "graph",
                format!(
                    "confirm the links steps 3 and 4 returned have `state` `wired`, the new \
                     node's `components.state` is `Running`, and link `{link_id}` is gone."
                ),
            ),
        ],
        closing_note: Some(format!(
            "If step 4 is refused because port `{target_port}` takes a single inbound link — an \
             audio input declaring a window contract does — run step 5 first, then step 4."
        )),
    })
}

fn fan_output_to_another_consumer_recipe(
    graph: &GraphResponse,
    prompt_arguments: &GraphRecipePromptArguments,
) -> std::result::Result<GraphRecipe, RpcError> {
    let (source, from_port) = output_port_named_by_arguments(graph, prompt_arguments)?;
    let processor_type = prompt_arguments.required("processor_type")?;

    Ok(GraphRecipe {
        context: format!(
            "Add a `{processor_type}` processor as another consumer of {} port `{from_port}`. \
             The links that port already feeds stay as they are.\n\n{}",
            node_label(source),
            catalog_context_for(processor_type)?
        ),
        steps: vec![
            add_processor_step(processor_type),
            find_the_added_node_step("its `ports.inputs`"),
            step(
                "connect",
                format!(
                    "`from_processor_id`: `{}`, `from_port`: `{from_port}`, `to_processor_id`: \
                     the new `processor_id`, `to_port`: its input port.",
                    source.id
                ),
            ),
            step(
                "graph",
                format!(
                    "confirm the link step 3 returned has `state` `wired`, the new node's \
                     `components.state` is `Running`, and {}'s other links are still there.",
                    node_label(source)
                ),
            ),
        ],
        closing_note: None,
    })
}

fn show_channel_on_virtual_camera_recipe(
    graph: &GraphResponse,
    prompt_arguments: &GraphRecipePromptArguments,
) -> std::result::Result<GraphRecipe, RpcError> {
    let (source, from_port) = output_port_named_by_arguments(graph, prompt_arguments)?;
    let camera_name = prompt_arguments.optional("camera_name")?;
    let virtual_camera_sink = catalog_entry_for(VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH)
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "this node's catalog has no `{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}`: \
                 the virtual camera is a Linux built-in"
            ))
        })?;
    let video_input_port = virtual_camera_sink
        .inputs
        .first()
        .map(|port| port.name.clone())
        .ok_or_else(|| {
            RpcError::internal(format!(
                "`{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}` is registered with no input \
                 port"
            ))
        })?;
    let config_instruction = match camera_name {
        Some(camera_name) => format!("`config`: `{}`", json!({ "name": camera_name })),
        None => "`config`: `{}`, which takes the default camera name".to_string(),
    };

    Ok(GraphRecipe {
        context: format!(
            "Present {} port `{from_port}` as a virtual camera: one camera every other \
             application on this machine can select, there while its processor runs and gone \
             when it is removed.\n\nThis node's catalog entry for it, read now:\n{}",
            node_label(source),
            pretty_json_block(&virtual_camera_sink)?
        ),
        steps: vec![
            step(
                "add_processor",
                format!(
                    "`type`: `{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}`; \
                     {config_instruction}. Keep the `processor_id` it returns."
                ),
            ),
            step(
                "connect",
                format!(
                    "`from_processor_id`: `{}`, `from_port`: `{from_port}`, `to_processor_id`: \
                     that `processor_id`, `to_port`: `{video_input_port}`.",
                    source.id
                ),
            ),
            step(
                "graph",
                "confirm the link step 2 returned has `state` `wired` and the camera's node's \
                 `components.state` is `Running`.",
            ),
        ],
        closing_note: Some(
            "The camera goes away with its processor: `remove_processor` with that \
             `processor_id`."
                .to_string(),
        ),
    })
}

fn look_at_what_a_channel_carries_recipe(
    graph: &GraphResponse,
    prompt_arguments: &GraphRecipePromptArguments,
) -> std::result::Result<GraphRecipe, RpcError> {
    let (source, from_port) = output_port_named_by_arguments(graph, prompt_arguments)?;
    let channel = source_channel_name(&source.id, &from_port).map_err(|e| {
        RpcError::invalid_params(format!(
            "processor `{}` port `{from_port}` names no channel: {e}",
            source.id
        ))
    })?;
    let port_key_bytes =
        FRAME_HEADER_SIZE - FRAME_HEADER_TIMESTAMP_BYTES - FRAME_HEADER_PAYLOAD_LENGTH_BYTES;

    Ok(GraphRecipe {
        context: format!(
            "Look at what {} publishes on port `{from_port}`, the channel `{}`.",
            node_label(source),
            channel.as_str()
        ),
        steps: vec![
            step(
                "tap",
                format!(
                    "`channel`: `{}`, `count`: 1. A bag's `hex_preview` is the channel's wire \
                     bytes: a {FRAME_HEADER_SIZE}-byte frame header — a {port_key_bytes}-byte \
                     port key, the producer's timestamp as {FRAME_HEADER_TIMESTAMP_BYTES} \
                     little-endian bytes of monotonic nanoseconds, then the payload length as \
                     {FRAME_HEADER_PAYLOAD_LENGTH_BYTES} little-endian bytes — followed by that \
                     many bytes of one msgpack map, the bag. `received` of 0 means nothing was \
                     published inside the sample window; tap again.",
                    channel.as_str()
                ),
            ),
            step(
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
