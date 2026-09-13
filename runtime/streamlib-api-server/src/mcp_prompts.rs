// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP prompts a node serves: recipes an agent follows with the tools the
//! node already serves, rendered against the live graph and the processor
//! catalog at the moment one is requested.
//!
//! A prompt is text, never a mutation path — every step it lists is a call to
//! a served tool, so the tool set stays the whole of the control vocabulary.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::iceoryx2::{
    FRAME_HEADER_PAYLOAD_LEN_SIZE, FRAME_HEADER_SIZE, FRAME_HEADER_TIMESTAMP_NS_SIZE,
    MAX_PORT_KEY_SIZE, source_channel_name,
};
use streamlib::sdk::json_schema::{
    GraphResponse, PortInfoOutput, ProcessorDescriptorOutput, ProcessorNodeOutput,
};
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::RuntimeOperations;

use crate::mcp::{RpcError, RpcResult};
use crate::mcp_resources::exported_live_graph_json;

/// The import path `VirtualCameraSink` registers under, which the virtual
/// camera recipe looks up in the catalog. This crate does not link the media
/// built-ins, so the wheel pins it against the built-in's own derived path.
pub const VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH: &str =
    "streamlib_media_builtins::virtual_camera_sink::VirtualCameraSink";

struct GraphRecipePromptArgument {
    name: &'static str,
    description: &'static str,
    required: bool,
}

type GraphRecipeRendering =
    fn(&GraphResponse, &GraphRecipePromptArguments) -> RpcResult<GraphRecipe>;

struct GraphRecipePromptDefinition {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    arguments: &'static [GraphRecipePromptArgument],
    render_recipe_against_live_graph: GraphRecipeRendering,
}

const LINK_ID_ARGUMENT: GraphRecipePromptArgument = GraphRecipePromptArgument {
    name: "link_id",
    description: "The id of the link to splice into, as `graph` lists it under `links`.",
    required: true,
};
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
const CAMERA_NAME_ARGUMENT: GraphRecipePromptArgument = GraphRecipePromptArgument {
    name: "camera_name",
    description: "The camera's name in every picker. Omit for the default name.",
    required: false,
};

const GRAPH_RECIPE_PROMPT_DEFINITIONS: &[GraphRecipePromptDefinition] = &[
    GraphRecipePromptDefinition {
        name: "insert_processor_between_linked_processors",
        title: "Insert a processor into a link",
        description: "Splice a new processor into an existing link, so what the link carried passes through it.",
        arguments: &[LINK_ID_ARGUMENT, PROCESSOR_TYPE_ARGUMENT],
        render_recipe_against_live_graph: insert_processor_between_linked_processors_recipe,
    },
    GraphRecipePromptDefinition {
        name: "fan_output_to_another_consumer",
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
        name: "show_channel_on_virtual_camera",
        title: "Show a channel on a virtual camera",
        description: "Present an output's video frames as a camera every other application on the machine can select.",
        arguments: &[
            FROM_PROCESSOR_ID_ARGUMENT,
            FROM_PORT_ARGUMENT,
            CAMERA_NAME_ARGUMENT,
        ],
        render_recipe_against_live_graph: show_channel_on_virtual_camera_recipe,
    },
    GraphRecipePromptDefinition {
        name: "look_at_what_a_channel_carries",
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
) -> RpcResult<Value> {
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
    let live_graph: GraphResponse =
        serde_json::from_value(exported_live_graph_json(runtime).await?)
            .map_err(|e| RpcError::internal(format!("graph export did not parse: {e}")))?;

    let recipe = (definition.render_recipe_against_live_graph)(&live_graph, &prompt_arguments)?;

    Ok(json!({
        "description": definition.description,
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": recipe.rendered_text() },
        }],
    }))
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

/// A prompt's string arguments, read through the declarations `prompts/list`
/// advertises and refused by name when a required one is absent.
struct GraphRecipePromptArguments {
    prompt_name: &'static str,
    arguments: Map<String, Value>,
}

impl GraphRecipePromptArguments {
    fn required(&self, argument: &GraphRecipePromptArgument) -> RpcResult<&str> {
        debug_assert!(
            argument.required,
            "`{}` is declared optional",
            argument.name
        );
        self.string_value(argument)?.ok_or_else(|| {
            RpcError::invalid_params(format!(
                "prompt `{}` needs the `{}` argument",
                self.prompt_name, argument.name
            ))
        })
    }

    fn optional(&self, argument: &GraphRecipePromptArgument) -> RpcResult<Option<&str>> {
        debug_assert!(
            !argument.required,
            "`{}` is declared required",
            argument.name
        );
        self.string_value(argument)
    }

    fn string_value(&self, argument: &GraphRecipePromptArgument) -> RpcResult<Option<&str>> {
        match self.arguments.get(argument.name) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.as_str())),
            Some(other) => Err(RpcError::invalid_params(format!(
                "prompt `{}` argument `{}` must be a string, got {other}",
                self.prompt_name, argument.name
            ))),
        }
    }
}

fn node_with_id<'graph>(
    graph: &'graph GraphResponse,
    processor_id: &str,
) -> RpcResult<&'graph ProcessorNodeOutput> {
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

fn input_port_of<'graph>(
    node: &'graph ProcessorNodeOutput,
    port_name: &str,
) -> Option<&'graph PortInfoOutput> {
    node.ports.inputs.iter().find(|port| port.name == port_name)
}

/// The processor `from_processor_id` names and the `from_port` the arguments
/// name, having checked that port is one of its outputs.
fn output_port_named_by_arguments<'graph, 'arguments>(
    graph: &'graph GraphResponse,
    prompt_arguments: &'arguments GraphRecipePromptArguments,
) -> RpcResult<(&'graph ProcessorNodeOutput, &'arguments str)> {
    let node = node_with_id(
        graph,
        prompt_arguments.required(&FROM_PROCESSOR_ID_ARGUMENT)?,
    )?;
    let from_port = prompt_arguments.required(&FROM_PORT_ARGUMENT)?;
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
    Ok((node, from_port))
}

fn processor_node_display_name_and_id_label(node: &ProcessorNodeOutput) -> String {
    format!("`{}` (id `{}`)", node.display_name, node.id)
}

/// The distinct delivery profiles the processors an output port feeds read it
/// under, leaving out the link a recipe is about to remove.
///
/// The engine keys a channel on its source output port and refuses a channel
/// whose destinations disagree on a profile, so these are the profiles a new
/// consumer of the port has to match.
fn delivery_profiles_an_output_port_keeps_feeding(
    graph: &GraphResponse,
    source_processor_id: &str,
    source_port: &str,
    link_id_being_removed: Option<&str>,
) -> Vec<String> {
    let mut delivery_profiles: Vec<String> = graph
        .links
        .iter()
        .filter(|link| {
            link.source.processor_id == source_processor_id
                && link.source.port_name == source_port
                && Some(link.id.as_str()) != link_id_being_removed
        })
        .filter_map(|link| {
            let consumer = graph
                .nodes
                .iter()
                .find(|node| node.id == link.target.processor_id)?;
            input_port_of(consumer, &link.target.port_name)?
                .delivery_profile
                .clone()
        })
        .collect();
    delivery_profiles.sort_unstable();
    delivery_profiles.dedup();
    delivery_profiles
}

/// The delivery profile a registered type's input declares, when it has
/// exactly one input for a recipe to wire.
fn sole_input_delivery_profile(entry: &ProcessorDescriptorOutput) -> Option<&str> {
    match entry.inputs.as_slice() {
        [sole_input] => sole_input.delivery_profile.as_deref(),
        _ => None,
    }
}

/// Refuses a recipe the engine would refuse at its `connect`: a new consumer
/// reading an output port under a profile a consumer the port keeps feeding
/// does not share.
fn refuse_a_new_consumer_whose_delivery_profile_conflicts(
    processor_type: &str,
    new_consumer_delivery_profile: Option<&str>,
    source: &ProcessorNodeOutput,
    source_port: &str,
    delivery_profiles_kept: &[String],
) -> RpcResult<()> {
    let Some(new_consumer_delivery_profile) = new_consumer_delivery_profile else {
        return Ok(());
    };
    if let Some(conflicting) = delivery_profiles_kept
        .iter()
        .find(|kept| kept.as_str() != new_consumer_delivery_profile)
    {
        return Err(RpcError::invalid_params(format!(
            "`{processor_type}` reads its input `{new_consumer_delivery_profile}`, but {} port \
             `{source_port}` also feeds a processor reading it `{conflicting}`, and every \
             consumer of one output port reads it under one delivery profile",
            processor_node_display_name_and_id_label(source)
        )));
    }
    Ok(())
}

/// The note for a type whose input profile the catalog cannot tell yet.
fn delivery_profile_refusal_note_for_an_unregistered_type(
    source: &ProcessorNodeOutput,
    source_port: &str,
    delivery_profiles_kept: &[String],
) -> Option<String> {
    let kept = delivery_profiles_kept.first()?;
    Some(format!(
        "{} port `{source_port}` feeds processors reading it `{kept}`, and every consumer of one \
         output port reads it under one delivery profile: a `connect` from it is refused naming \
         conflicting delivery profiles when the new processor's input declares another.",
        processor_node_display_name_and_id_label(source)
    ))
}

/// The catalog entry for one import path, or `None` when nothing is
/// registered under it — a string that is not an import path included.
fn catalog_entry_for(processor_type: &str) -> Option<ProcessorDescriptorOutput> {
    let processor_class_import_path = ProcessorClassImportPath::new(processor_type).ok()?;
    PROCESSOR_REGISTRY
        .descriptor(&processor_class_import_path)
        .map(|descriptor| ProcessorDescriptorOutput::from(&descriptor))
}

fn catalog_entry_json_block(entry: &ProcessorDescriptorOutput) -> RpcResult<String> {
    let text = serde_json::to_string_pretty(entry)
        .map_err(|e| RpcError::internal(format!("catalog entry rendering failed: {e}")))?;
    Ok(format!("```json\n{text}\n```"))
}

/// What the catalog says about a type an agent is about to add, or why it says
/// nothing yet.
fn catalog_introduction_for(
    processor_type: &str,
    entry: Option<&ProcessorDescriptorOutput>,
) -> RpcResult<String> {
    match entry {
        Some(entry) => Ok(format!(
            "This node's catalog entry for `{processor_type}`, read now — `config_schema` is \
             what `add_processor`'s `config` takes, and `inputs` and `outputs` are its ports:\n{}",
            catalog_entry_json_block(entry)?
        )),
        None => Ok(format!(
            "`{processor_type}` is not in this node's catalog yet. A Python class enters it when \
             its module is imported, which `add_processor` does; its ports then show in `graph`, \
             and its config takes the keys its `__init__`'s config class declares."
        )),
    }
}

fn add_processor_step(processor_type: &str) -> GraphRecipeStep {
    graph_recipe_step_calling_tool(
        "add_processor",
        format!(
            "`type`: `{processor_type}`; `config`: an object of the keys its config schema \
             declares, or omit it when there are none. Keep the `processor_id` it returns."
        ),
    )
}

fn find_the_added_node_step(port_directions: &str) -> GraphRecipeStep {
    graph_recipe_step_calling_tool(
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
) -> RpcResult<GraphRecipe> {
    let link_id = prompt_arguments.required(&LINK_ID_ARGUMENT)?;
    let processor_type = prompt_arguments.required(&PROCESSOR_TYPE_ARGUMENT)?;
    let link = graph
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
    let source_port = link.source.port_name.as_str();
    let target_port = link.target.port_name.as_str();
    let source_label = processor_node_display_name_and_id_label(source);
    let target_label = processor_node_display_name_and_id_label(target);

    let inserted_type_entry = catalog_entry_for(processor_type);
    let inserted_delivery_profile = inserted_type_entry
        .as_ref()
        .and_then(sole_input_delivery_profile);
    let delivery_profiles_kept = delivery_profiles_an_output_port_keeps_feeding(
        graph,
        &source.id,
        source_port,
        Some(link_id),
    );
    refuse_a_new_consumer_whose_delivery_profile_conflicts(
        processor_type,
        inserted_delivery_profile,
        source,
        source_port,
        &delivery_profiles_kept,
    )?;

    let target_input = input_port_of(target, target_port);
    let target_delivery_profile = target_input.and_then(|port| port.delivery_profile.as_deref());
    let reason_the_link_goes_first = if target_input.is_some_and(|port| port.audio_window.is_some())
    {
        Some(format!(
            "{target_label} port `{target_port}` declares an audio window contract, so it takes a \
             single inbound link"
        ))
    } else {
        match (inserted_delivery_profile, target_delivery_profile) {
            (Some(inserted), Some(replaced)) if inserted != replaced => Some(format!(
                "`{processor_type}` reads its input `{inserted}` while {target_label} reads port \
                 `{target_port}` `{replaced}`, and every consumer of {source_label} port \
                 `{source_port}` reads it under one delivery profile"
            )),
            _ => None,
        }
    };

    let connect_source_to_inserted = graph_recipe_step_calling_tool(
        "connect",
        format!(
            "`from_processor_id`: `{}`, `from_port`: `{source_port}`, `to_processor_id`: the new \
             `processor_id`, `to_port`: its input port.",
            source.id
        ),
    );
    let connect_inserted_to_target = graph_recipe_step_calling_tool(
        "connect",
        format!(
            "`from_processor_id`: the new `processor_id`, `from_port`: its output port, \
             `to_processor_id`: `{}`, `to_port`: `{target_port}`.",
            target.id
        ),
    );
    let disconnect_the_replaced_link =
        graph_recipe_step_calling_tool("disconnect", format!("`link_id`: `{link_id}`."));
    let wiring_steps = if reason_the_link_goes_first.is_some() {
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
        add_processor_step(processor_type),
        find_the_added_node_step("its `ports.inputs` and `ports.outputs`"),
    ];
    steps.extend(wiring_steps);
    steps.push(graph_recipe_step_calling_tool(
        "graph",
        format!(
            "confirm the links both `connect` calls returned have `state` `wired`, the new node's \
             `components.state` is `Running`, and link `{link_id}` is gone."
        ),
    ));

    let closing_note = match (&reason_the_link_goes_first, &inserted_type_entry) {
        (Some(reason), _) => Some(format!(
            "The link goes before the new processor is wired because {reason}; {target_label} \
             receives nothing between the `disconnect` and the second `connect`. If a `connect` \
             is refused, `connect` {source_label} port `{source_port}` to {target_label} port \
             `{target_port}` again to restore what the link carried."
        )),
        (None, Some(_)) => None,
        (None, None) => {
            let mut note = "If a `connect` is refused naming conflicting delivery profiles, or \
                            naming a port that takes a single inbound link, call `disconnect` \
                            before the first `connect` instead."
                .to_string();
            if let Some(delivery_profile_note) =
                delivery_profile_refusal_note_for_an_unregistered_type(
                    source,
                    source_port,
                    &delivery_profiles_kept,
                )
            {
                note.push(' ');
                note.push_str(&delivery_profile_note);
            }
            Some(note)
        }
    };

    Ok(GraphRecipe {
        introduction_text: format!(
            "Insert a `{processor_type}` processor into link `{link_id}`, which carries \
             {source_label} port `{source_port}` to {target_label} port `{target_port}`.\n\n{}",
            catalog_introduction_for(processor_type, inserted_type_entry.as_ref())?
        ),
        steps,
        closing_note,
    })
}

fn fan_output_to_another_consumer_recipe(
    graph: &GraphResponse,
    prompt_arguments: &GraphRecipePromptArguments,
) -> RpcResult<GraphRecipe> {
    let (source, from_port) = output_port_named_by_arguments(graph, prompt_arguments)?;
    let processor_type = prompt_arguments.required(&PROCESSOR_TYPE_ARGUMENT)?;
    let source_label = processor_node_display_name_and_id_label(source);
    let consumer_type_entry = catalog_entry_for(processor_type);
    let delivery_profiles_kept =
        delivery_profiles_an_output_port_keeps_feeding(graph, &source.id, from_port, None);
    refuse_a_new_consumer_whose_delivery_profile_conflicts(
        processor_type,
        consumer_type_entry
            .as_ref()
            .and_then(sole_input_delivery_profile),
        source,
        from_port,
        &delivery_profiles_kept,
    )?;

    Ok(GraphRecipe {
        introduction_text: format!(
            "Add a `{processor_type}` processor as another consumer of {source_label} port \
             `{from_port}`. The links that port already feeds stay as they are.\n\n{}",
            catalog_introduction_for(processor_type, consumer_type_entry.as_ref())?
        ),
        steps: vec![
            add_processor_step(processor_type),
            find_the_added_node_step("its `ports.inputs`"),
            graph_recipe_step_calling_tool(
                "connect",
                format!(
                    "`from_processor_id`: `{}`, `from_port`: `{from_port}`, `to_processor_id`: \
                     the new `processor_id`, `to_port`: its input port.",
                    source.id
                ),
            ),
            graph_recipe_step_calling_tool(
                "graph",
                format!(
                    "confirm the link `connect` returned has `state` `wired`, the new node's \
                     `components.state` is `Running`, and {source_label}'s other links are still \
                     there."
                ),
            ),
        ],
        closing_note: match consumer_type_entry {
            Some(_) => None,
            None => delivery_profile_refusal_note_for_an_unregistered_type(
                source,
                from_port,
                &delivery_profiles_kept,
            ),
        },
    })
}

fn show_channel_on_virtual_camera_recipe(
    graph: &GraphResponse,
    prompt_arguments: &GraphRecipePromptArguments,
) -> RpcResult<GraphRecipe> {
    let (source, from_port) = output_port_named_by_arguments(graph, prompt_arguments)?;
    let camera_name = prompt_arguments.optional(&CAMERA_NAME_ARGUMENT)?;
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
        .map(|port| port.name.as_str())
        .ok_or_else(|| {
            RpcError::internal(format!(
                "`{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}` is registered with no input \
                 port"
            ))
        })?;
    refuse_a_new_consumer_whose_delivery_profile_conflicts(
        VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
        sole_input_delivery_profile(&virtual_camera_sink),
        source,
        from_port,
        &delivery_profiles_an_output_port_keeps_feeding(graph, &source.id, from_port, None),
    )?;
    let config_instruction = match camera_name {
        Some(camera_name) => format!("`config`: `{}`", json!({ "name": camera_name })),
        None => "`config`: `{}`, which takes the default camera name".to_string(),
    };

    Ok(GraphRecipe {
        introduction_text: format!(
            "Present {} port `{from_port}` as a virtual camera: one camera every other \
             application on this machine can select, there while its processor runs and gone \
             when it is removed.\n\nThis node's catalog entry for it, read now:\n{}",
            processor_node_display_name_and_id_label(source),
            catalog_entry_json_block(&virtual_camera_sink)?
        ),
        steps: vec![
            graph_recipe_step_calling_tool(
                "add_processor",
                format!(
                    "`type`: `{VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH}`; \
                     {config_instruction}. Keep the `processor_id` it returns."
                ),
            ),
            graph_recipe_step_calling_tool(
                "connect",
                format!(
                    "`from_processor_id`: `{}`, `from_port`: `{from_port}`, `to_processor_id`: \
                     that `processor_id`, `to_port`: `{video_input_port}`.",
                    source.id
                ),
            ),
            graph_recipe_step_calling_tool(
                "graph",
                "confirm the link `connect` returned has `state` `wired` and the camera's node's \
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
) -> RpcResult<GraphRecipe> {
    let (source, from_port) = output_port_named_by_arguments(graph, prompt_arguments)?;
    let channel = source_channel_name(&source.id, from_port).map_err(|e| {
        RpcError::invalid_params(format!(
            "processor `{}` port `{from_port}` names no channel: {e}",
            source.id
        ))
    })?;

    Ok(GraphRecipe {
        introduction_text: format!(
            "Look at what {} publishes on port `{from_port}`, the channel `{}`.",
            processor_node_display_name_and_id_label(source),
            channel.as_str()
        ),
        steps: vec![
            graph_recipe_step_calling_tool(
                "tap",
                format!(
                    "`channel`: `{}`, `count`: 1. A bag's `hex_preview` is the channel's wire \
                     bytes: a {FRAME_HEADER_SIZE}-byte frame header — a {MAX_PORT_KEY_SIZE}-byte \
                     port key, the producer's timestamp as {FRAME_HEADER_TIMESTAMP_NS_SIZE} \
                     little-endian bytes of monotonic nanoseconds, then the payload length as \
                     {FRAME_HEADER_PAYLOAD_LEN_SIZE} little-endian bytes — followed by that many \
                     bytes of one msgpack map, the bag. `received` of 0 means nothing was \
                     published inside the sample window; tap again.",
                    channel.as_str()
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
