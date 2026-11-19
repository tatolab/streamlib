// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Resources and prompts: the server instructions, the catalog and live-graph resources, the
//! wiring prompts, and the tool each prompt step calls.

use super::*;
use streamlib::sdk::descriptors::{NodeClassShortName, NodeDescriptor, PortDescriptor};
use streamlib::sdk::nodes::NODE_REGISTRY;

/// The control vocabulary tells an agent to confirm `wired` in `graph`. A
/// link onto a helper is not wired when `connect` returns, so the same text
/// has to say what `pending` and `error` mean or the confirmation is a
/// guess.
#[tokio::test]
async fn the_instructions_and_every_wiring_prompt_say_what_pending_and_error_mean() {
    register_a_virtual_camera_sink_probe_once();
    let mut texts = vec![(
        "instructions",
        LOCAL_API_MCP_SERVER_INSTRUCTIONS.to_string(),
    )];
    for (recipe, arguments) in [
        (
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
        ),
        (
            "fan_output_to_another_consumer",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
        ),
        (
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video" }),
        ),
    ] {
        texts.push((
            recipe,
            prompt_text(stub_serving_two_linked_nodes(), recipe, arguments).await,
        ));
    }
    for (name, text) in texts {
        assert!(
            text.contains("pending") && text.contains("error_reason"),
            "{name} tells an agent to confirm a link's state but not what a link left \
             `pending` by a helper that has not answered, or refused with a reason, \
             means:\n{text}"
        );
    }
}

fn stub_serving_two_linked_nodes() -> Arc<ControlPlaneMcpDispatchStubRuntime> {
    let runtime = ControlPlaneMcpDispatchStubRuntime::new();
    *runtime.exported_graph.lock() = two_linked_nodes_graph();
    Arc::new(runtime)
}

/// The registry is process-global and refuses a second registration of a
/// path, so the probe standing in for the built-in registers once for the
/// whole test binary.
fn register_a_virtual_camera_sink_probe_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        NODE_REGISTRY
            .register_descriptor_only(
                NodeDescriptor::new(
                    NodeClassShortName::new("VirtualCameraSink").unwrap(),
                    NodeClassImportPath::new(
                        crate::mcp_prompts::VIRTUAL_CAMERA_SINK_NODE_CLASS_IMPORT_PATH,
                    )
                    .unwrap(),
                    "a virtual-camera probe",
                )
                .with_config_schema(
                    json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
                )
                .with_input(
                    PortDescriptor::new(
                        "video",
                        "frames to present",
                        true,
                        streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                            streamlib::sdk::descriptors::PortFamily::Image,
                        ),
                    )
                    .with_delivery_profile("newest"),
                ),
            )
            .expect("the virtual camera path is registered by this helper alone");
    });
}

async fn listed_resources_result(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> Value {
    let (_served, client) = connected_mcp_client(runtime).await;
    wire_outcome(client.list_resources(None).await).unwrap()
}

async fn listed_resource_templates_result(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> Value {
    let (_served, client) = connected_mcp_client(runtime).await;
    wire_outcome(client.list_resource_templates(None).await).unwrap()
}

async fn listed_prompts_result(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> Value {
    let (_served, client) = connected_mcp_client(runtime).await;
    wire_outcome(client.list_prompts(None).await).unwrap()
}

/// The tools a recipe's numbered steps call, in order — what a client
/// following the text step by step dispatches.
fn tool_names_the_numbered_steps_call(prompt_text: &str) -> Vec<String> {
    prompt_text
        .lines()
        .filter_map(|line| {
            let (step_number, rest) = line.split_once(". `")?;
            if step_number.is_empty() || !step_number.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            rest.split_once('`')
                .map(|(tool_name, _)| tool_name.to_string())
        })
        .collect()
}

fn served_tool_names() -> Vec<String> {
    streamlib_control_contract::the_operations_served_to_agents()
        .map(|declaration| declaration.method_name.to_owned())
        .collect()
}

#[tokio::test]
async fn resources_list_names_the_node_catalog_the_live_graph_and_the_actions_under_way() {
    let result = listed_resources_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;

    let uris: Vec<&str> = result["resources"]
        .as_array()
        .expect("a resources array")
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        [
            "streamlib://node-catalog",
            "streamlib://graph",
            "streamlib://stream-actions"
        ]
    );
    for resource in result["resources"].as_array().unwrap() {
        assert_eq!(resource["mimeType"], "application/json", "{resource}");
        assert!(
            resource["description"]
                .as_str()
                .is_some_and(|d| !d.is_empty())
        );
    }

    let templates =
        listed_resource_templates_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
    let uri_templates: Vec<&str> = templates["resourceTemplates"]
        .as_array()
        .expect("a resource templates array")
        .iter()
        .map(|template| template["uriTemplate"].as_str().unwrap())
        .collect();
    assert_eq!(
        uri_templates,
        [
            "streamlib://project/{project}/transactions",
            "streamlib://stream-action/{action}"
        ],
        "{templates}"
    );
}

/// Mental revert: render the catalog once at startup and cache it, and the
/// second read misses the type registered after the first.
#[tokio::test]
async fn the_catalog_resource_renders_the_registry_as_it_stands_at_each_read() {
    let class_import_path = "streamlib_api_server::catalog_resource_probe::RegisteredBetweenReads";
    let entry_for_the_probe = |catalog: &Value| {
        catalog["nodes"]
            .as_array()
            .expect("a node type list")
            .iter()
            .find(|entry| entry["type"] == class_import_path)
            .cloned()
    };

    let before = resource_document(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "streamlib://node-catalog",
    )
    .await;
    assert!(entry_for_the_probe(&before).is_none());

    let config_schema = json!({
        "type": "object",
        "properties": { "strength": { "type": "number", "default": 0.5 } }
    });
    NODE_REGISTRY
        .register_descriptor_only(
            NodeDescriptor::new(
                NodeClassShortName::new("RegisteredBetweenReads").unwrap(),
                NodeClassImportPath::new(class_import_path).unwrap(),
                "registered after the first read",
            )
            .with_config_schema(config_schema.clone())
            .with_input(
                PortDescriptor::new(
                    "video_from_upstream",
                    "",
                    true,
                    streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                        streamlib::sdk::descriptors::PortFamily::Image,
                    ),
                )
                .with_delivery_profile("newest"),
            ),
        )
        .expect("the probe's path is registered by this test alone");

    let after = resource_document(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "streamlib://node-catalog",
    )
    .await;
    let entry = entry_for_the_probe(&after).expect("the type registered between reads");
    assert_eq!(entry["config_schema"], config_schema);
    assert_eq!(entry["inputs"][0]["name"], "video_from_upstream");
    assert_eq!(entry["inputs"][0]["delivery_profile"], "newest");
    assert_eq!(
        after["streams"],
        json!([{ "stream": STUB_STREAM_ID, "nodes": [], "installed": [] }]),
        "each loaded stream lists the types its own interpreter described"
    );
}

/// The graph resource is every loaded stream's graph under the runtime's
/// name, rendered at each read.
#[tokio::test]
async fn the_graph_resource_renders_every_loaded_streams_graph_as_it_stands_at_each_read() {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let exported_graph = runtime.exported_graph.clone();

    let before = resource_document(runtime.clone(), "streamlib://graph").await;
    assert_eq!(
        before,
        json!({
            "machine": STUB_MACHINE_NAME,
            "streams": [{ "nodes": [], "links": [] }],
        })
    );

    *exported_graph.lock() = two_linked_nodes_graph();
    let after = resource_document(runtime, "streamlib://graph").await;
    assert_eq!(
        after,
        json!({
            "machine": STUB_MACHINE_NAME,
            "streams": [as_a_graph_call_answers_it(two_linked_nodes_graph())],
        })
    );
}

#[tokio::test]
async fn reading_a_resource_the_node_does_not_serve_is_refused_naming_the_uri() {
    let refusal = resource_read_outcome(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "streamlib://contracts",
    )
    .await
    .expect_err("an unserved uri is refused");
    assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        refusal.message.contains("streamlib://contracts"),
        "{refusal:?}"
    );
}

#[tokio::test]
async fn prompts_list_names_the_three_recipes_and_their_arguments() {
    let result = listed_prompts_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
    let prompts = result["prompts"].as_array().expect("a prompts array");

    let described: Vec<(String, Vec<(String, bool)>)> = prompts
        .iter()
        .map(|prompt| {
            let arguments = prompt["arguments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|argument| {
                    (
                        argument["name"].as_str().unwrap().to_string(),
                        argument["required"].as_bool().unwrap(),
                    )
                })
                .collect();
            (prompt["name"].as_str().unwrap().to_string(), arguments)
        })
        .collect();
    let owned = |name: &str, arguments: &[(&str, bool)]| {
        (
            name.to_string(),
            arguments
                .iter()
                .map(|(argument, required)| (argument.to_string(), *required))
                .collect::<Vec<_>>(),
        )
    };
    let mut expected = vec![
        owned(
            "insert_node_between_linked_nodes",
            &[("stream", true), ("link_id", true), ("type", true)],
        ),
        owned(
            "fan_output_to_another_consumer",
            &[
                ("stream", true),
                ("from_node", true),
                ("from_port", true),
                ("type", true),
            ],
        ),
        owned(
            "show_channel_on_virtual_camera",
            &[
                ("stream", true),
                ("from_node", true),
                ("from_port", true),
                ("camera_name", false),
            ],
        ),
    ];
    let mut described = described;
    described.sort();
    expected.sort();
    assert_eq!(described, expected);
}

/// A prompt is a recipe over the tool set, never a verb of its own: every
/// step of every recipe calls a tool `tools/list` serves.
#[tokio::test]
async fn every_step_of_every_prompt_calls_a_tool_the_node_serves() {
    register_a_virtual_camera_sink_probe_once();
    let served = served_tool_names();

    for (prompt_name, arguments) in [
        (
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
        ),
        (
            "fan_output_to_another_consumer",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
        ),
        (
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video" }),
        ),
    ] {
        let text = prompt_text(stub_serving_two_linked_nodes(), prompt_name, arguments).await;
        let called = tool_names_the_numbered_steps_call(&text);
        assert!(!called.is_empty(), "{prompt_name} lists no steps:\n{text}");
        for tool_name in &called {
            assert!(
                served.contains(tool_name),
                "{prompt_name} calls `{tool_name}`, which the node does not serve:\n{text}"
            );
        }
    }
}

#[tokio::test]
async fn the_insert_prompt_splices_the_named_link_in_an_order_that_never_leaves_it_unfed() {
    let text = prompt_text(
        stub_serving_two_linked_nodes(),
        "insert_node_between_linked_nodes",
        json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
    )
    .await;

    assert_eq!(
        tool_names_the_numbered_steps_call(&text),
        [
            "add_node",
            "graph",
            "connect",
            "connect",
            "disconnect",
            "graph"
        ]
    );
    for named in [
        "`effects:Blur`",
        "`pattern`",
        "`window`",
        "`link-pattern-to-window`",
    ] {
        assert!(
            text.contains(named),
            "the recipe must name {named}:\n{text}"
        );
    }
}

/// The type an agent is about to add is described from the catalog as it
/// is now, so the config keys it passes are the ones the node will take.
#[tokio::test]
async fn a_registered_types_catalog_entry_is_rendered_into_the_prompt() {
    let class_import_path = "streamlib_api_server::prompt_catalog_probe::GrayscaleEffect";
    NODE_REGISTRY
        .register_descriptor_only(
            NodeDescriptor::new(
                NodeClassShortName::new("GrayscaleEffect").unwrap(),
                NodeClassImportPath::new(class_import_path).unwrap(),
                "a prompt-rendering probe",
            )
            .with_config_schema(json!({
                "type": "object",
                "properties": { "grayscale_strength_probe_key": { "type": "number" } }
            })),
        )
        .expect("the probe's path is registered by this test alone");

    let registered = prompt_text(
        stub_serving_two_linked_nodes(),
        "fan_output_to_another_consumer",
        json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": class_import_path }),
    )
    .await;
    assert!(
        registered.contains("grayscale_strength_probe_key"),
        "a registered type's config schema must reach the recipe:\n{registered}"
    );

    let unregistered = prompt_text(
        stub_serving_two_linked_nodes(),
        "fan_output_to_another_consumer",
        json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "never_imported:Effect" }),
    )
    .await;
    assert!(
        unregistered.contains("not in this node's catalog yet"),
        "an unregistered type must be said to be absent, not described:\n{unregistered}"
    );
}

#[tokio::test]
async fn the_virtual_camera_prompt_adds_the_registered_sink_on_its_own_input_port() {
    register_a_virtual_camera_sink_probe_once();

    let text = prompt_text(
        stub_serving_two_linked_nodes(),
        "show_channel_on_virtual_camera",
        json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "camera_name": "Desk \"cam\"" }),
    )
    .await;

    assert_eq!(
        tool_names_the_numbered_steps_call(&text),
        ["add_node", "connect", "graph"]
    );
    assert!(
        text.contains(crate::mcp_prompts::VIRTUAL_CAMERA_SINK_NODE_CLASS_IMPORT_PATH),
        "{text}"
    );
    assert!(
        text.contains("`target`: that UUID and the port `video`"),
        "{text}"
    );
    assert!(
        text.contains(r#"{"name":"Desk \"cam\""}"#),
        "the camera name must reach the config as escaped JSON:\n{text}"
    );
}

#[tokio::test]
async fn a_prompt_request_naming_nothing_the_node_has_is_refused_by_name() {
    for (case, prompt_name, arguments, named) in [
        (
            "an unknown prompt",
            "delete_everything",
            json!({}),
            "delete_everything",
        ),
        (
            "a missing required argument",
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window" }),
            "`type`",
        ),
        (
            "a missing stream",
            "show_channel_on_virtual_camera",
            json!({ "from_node": "pattern", "from_port": "video" }),
            "`stream`",
        ),
        (
            "a stream not loaded",
            "show_channel_on_virtual_camera",
            json!({ "stream": "elsewhere", "from_node": "pattern", "from_port": "video" }),
            STUB_STREAM_ID,
        ),
        (
            "a link the graph does not have",
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "no-such-link", "type": "effects:Blur" }),
            "no-such-link",
        ),
        (
            "a node the graph does not have",
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": "nosuchnode", "from_port": "video" }),
            "nosuchnode",
        ),
        (
            "a port that is not one of the node's outputs",
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": "window", "from_port": "video" }),
            "video",
        ),
        (
            "an argument that is not a string",
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": 7, "from_port": "video" }),
            "7",
        ),
        (
            "a misspelled argument",
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "camera_nme": "desk" }),
            "camera_nme",
        ),
    ] {
        let refusal = prompt_outcome(stub_serving_two_linked_nodes(), prompt_name, arguments)
            .await
            .expect_err(case);
        assert_eq!(
            refusal.code,
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "{case}: {refusal:?}"
        );
        assert!(
            refusal.message.contains(named),
            "{case} must be refused naming `{named}`: {refusal:?}"
        );
    }
}

#[tokio::test]
async fn the_fan_prompt_adds_one_consumer_and_wires_it_to_the_named_port() {
    let text = prompt_text(
        stub_serving_two_linked_nodes(),
        "fan_output_to_another_consumer",
        json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
    )
    .await;

    assert_eq!(
        tool_names_the_numbered_steps_call(&text),
        ["add_node", "graph", "connect", "graph"]
    );
    assert!(
        text.contains(r#"`source`: `{"node": "PatternSourceId", "port": "video"}`"#),
        "{text}"
    );
}

/// A consumer reading another profile than the ones an output port already
/// feeds is fanned like any other. Mental revert: restore the refusal and
/// the recipe is an error naming both profiles.
#[tokio::test]
async fn the_fan_prompt_wires_a_consumer_reading_another_profile_than_the_port_already_feeds() {
    let ordered_input_probe = register_a_sole_input_probe_once("ordered");

    let text = prompt_text(
        stub_serving_two_linked_nodes(),
        "fan_output_to_another_consumer",
        json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": ordered_input_probe }),
    )
    .await;

    assert_eq!(
        tool_names_the_numbered_steps_call(&text),
        ["add_node", "graph", "connect", "graph"]
    );
}

/// The virtual camera's `newest` input joins a port whatever its other
/// consumers read. Mental revert: restore the refusal and a port already
/// feeding an `ordered` consumer refuses the recipe.
#[tokio::test]
async fn the_virtual_camera_prompt_wires_onto_a_port_that_feeds_an_ordered_consumer() {
    register_a_virtual_camera_sink_probe_once();
    let runtime = ControlPlaneMcpDispatchStubRuntime::new();
    let mut graph = two_linked_nodes_graph();
    graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
    *runtime.exported_graph.lock() = graph;

    let text = prompt_text(
        Arc::new(runtime),
        "show_channel_on_virtual_camera",
        json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video" }),
    )
    .await;

    assert_eq!(
        tool_names_the_numbered_steps_call(&text),
        ["add_node", "connect", "graph"]
    );
}

/// A probe with one input declaring `delivery_profile`, registered once per
/// profile for the test binary.
fn register_a_sole_input_probe_once(delivery_profile: &'static str) -> String {
    static REGISTERED_PROFILES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let class_import_path = format!(
        "streamlib_api_server::prompt_delivery_profile_probe::{delivery_profile}::SoleInputProbe"
    );
    let mut registered_profiles = REGISTERED_PROFILES.lock();
    if !registered_profiles.contains(&delivery_profile) {
        NODE_REGISTRY
            .register_descriptor_only(
                NodeDescriptor::new(
                    NodeClassShortName::new("SoleInputProbe").unwrap(),
                    NodeClassImportPath::new(&class_import_path).unwrap(),
                    "a probe with one input",
                )
                .with_input(
                    PortDescriptor::new(
                        "bags_from_upstream",
                        "",
                        true,
                        streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                            streamlib::sdk::descriptors::PortFamily::Image,
                        ),
                    )
                    .with_delivery_profile(delivery_profile),
                )
                .with_output(PortDescriptor::new(
                    "bags_to_downstream",
                    "",
                    true,
                    streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                        streamlib::sdk::descriptors::PortFamily::Image,
                    ),
                )),
            )
            .expect("each probe path is registered by this helper alone");
        registered_profiles.push(delivery_profile);
    }
    class_import_path
}

/// Consumers of one output port read it under whatever profiles they each
/// declare, so a type reading another profile than the link it is spliced
/// into still takes the zero-gap order: both new links first, the old one
/// last. Mental revert: take the link out first again and the target sees a
/// gap no refusal calls for.
#[tokio::test]
async fn inserting_a_type_that_reads_another_profile_than_the_link_connects_before_it_disconnects()
{
    for (inserted_profile, replaced_profile) in [("newest", "ordered"), ("ordered", "newest")] {
        let inserted_probe = register_a_sole_input_probe_once(inserted_profile);
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        let mut graph = two_linked_nodes_graph();
        graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!(replaced_profile);
        *runtime.exported_graph.lock() = graph;

        let text = prompt_text(
            Arc::new(runtime),
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": inserted_probe }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_node",
                "graph",
                "connect",
                "connect",
                "disconnect",
                "graph"
            ],
            "a `{inserted_profile}` type into a `{replaced_profile}` link:\n{text}"
        );
    }
}

/// A class the agent just wrote is not in the catalog until its first add,
/// and nothing about the recipe waits on it: the order is the zero-gap one
/// and no note asks the agent to read a profile before wiring.
#[tokio::test]
async fn an_insert_of_an_uncatalogued_type_connects_before_it_disconnects_with_no_profile_note() {
    let text = prompt_text(
        stub_serving_two_linked_nodes(),
        "insert_node_between_linked_nodes",
        json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:WrittenJustNow" }),
    )
    .await;

    assert_eq!(
        tool_names_the_numbered_steps_call(&text),
        [
            "add_node",
            "graph",
            "connect",
            "connect",
            "disconnect",
            "graph"
        ]
    );
    assert!(
        !text.contains("delivery profile") && !text.contains("remove_node"),
        "nothing about the insert depends on the new type's profile:\n{text}"
    );
}

#[tokio::test]
async fn inserting_into_a_link_whose_target_takes_one_inbound_link_removes_the_link_first() {
    let runtime = ControlPlaneMcpDispatchStubRuntime::new();
    let mut graph = two_linked_nodes_graph();
    graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
    graph["nodes"][1]["ports"]["inputs"][0]["audio_window"] =
        json!({ "resolved_from": "match_device" });
    *runtime.exported_graph.lock() = graph;

    let text = prompt_text(
        Arc::new(runtime),
        "insert_node_between_linked_nodes",
        json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:NeverImported" }),
    )
    .await;

    assert_eq!(
        tool_names_the_numbered_steps_call(&text),
        [
            "add_node",
            "graph",
            "disconnect",
            "connect",
            "connect",
            "graph"
        ]
    );
    assert!(text.contains("audio window contract"), "{text}");
}
