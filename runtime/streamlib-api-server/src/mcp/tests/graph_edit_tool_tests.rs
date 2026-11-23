// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The graph edit tools: adding, connecting and disconnecting nodes, `transact`, what each edit
//! carries, and the edits refused.

use super::*;

const STUB_CAMERA_NODE_UUID: &str = "11111111-1111-4111-8111-111111111111";

const STUB_EFFECT_NODE_UUID: &str = "22222222-2222-4222-8222-222222222222";

/// A stub whose graph holds a camera linked to an effect, each by its UUID.
fn a_stub_holding_a_camera_linked_to_an_effect() -> Arc<ControlPlaneMcpDispatchStubRuntime> {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    *runtime.exported_graph.lock() = json!({
        "machine": crate::control_plane_stub_support::STUB_MACHINE_NAME,
        "nodes": [
            { "id": STUB_CAMERA_NODE_UUID, "name": "camera", "type": "tatolab.camera:Camera",
              "input_values": { "fps": 30 }, "studio": { "position": [0, 0] } },
            { "id": STUB_EFFECT_NODE_UUID, "name": "fx", "type": "nodes.fx:Fx",
              "input_values": { "strength": 0.5 } },
        ],
        "links": [
            { "id": "link-9", "source": { "node": "camera", "port": "video" },
              "target": { "node": "fx", "port": "video_in" }, "state": "pending" },
        ],
    });
    runtime
}

#[tokio::test]
async fn tools_call_add_node_applies_the_node_live_and_answers_its_new_uuid_and_name() {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let recorded = runtime.recorded_graph_mutations.clone();

    let stated = first_text_block_json(
        &tool_call_result(
            runtime,
            "add_node",
            json!({
                "stream": STUB_STREAM_ASSET,
                "type": "nodes.grayscale_effect:GrayscaleEffect",
                "config": { "strength": 0.5 },
                "name": "Gray",
                "mode": "live",
            }),
        )
        .await,
    );

    let [streamlib_project_repository::StreamGraphOperation::AddNode { node, links }] =
        &recorded_stream_graph_operations(&recorded)[..]
    else {
        panic!(
            "add_node reaches exactly one add, recorded {:?}",
            recorded.lock()
        );
    };
    assert_eq!(node.node_type, "nodes.grayscale_effect:GrayscaleEffect");
    assert_eq!(
        Value::Object(node.input_values.clone()),
        json!({ "strength": 0.5 })
    );
    assert_eq!(node.name, "Gray");
    assert!(links.is_empty());
    assert_eq!(stated["mode"], "live");
    assert_eq!(
        stated["transaction"],
        Value::Null,
        "a live edit records nothing"
    );
    assert_eq!(
        stated["added_nodes"],
        json!([{ "stream": STUB_STREAM_ID, "node": node.id.to_string(), "name": "gray" }]),
        "the answer is the UUID the runtime made and the name the engine cast"
    );
}

/// What this locks is the door: an engine that refuses an add — a name
/// already taken, an unknown class — reaches the MCP caller as a tool error
/// carrying its own words, rather than as a success or a swallowed reason.
#[tokio::test]
async fn tools_call_add_node_carries_back_an_engine_refusal_in_its_own_words() {
    let refusal = "the node name `gray` is already taken in this graph";
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::refusing_every_add_node(
        refusal,
    ));

    let body = tool_call_result(
        runtime,
        "add_node",
        json!({
            "stream": STUB_STREAM_ASSET,
            "type": "nodes.grayscale_effect:GrayscaleEffect",
            "name": "gray",
            "mode": "live",
        }),
    )
    .await;
    let refused = the_refusal_of_a_tool_result(&body);
    assert_eq!(refused.action, "add_node");
    let text = refused.runtime_words;
    assert!(
        text.contains(refusal),
        "the refusal must reach the caller in the engine's own words: {text}"
    );
}

/// An engine's typed refusal of a live add keeps its code through the transaction: an unknown
/// node type answers `node_type_not_found`, a name another node has `node_name_taken`.
#[tokio::test]
async fn a_transact_whose_live_add_the_engine_refuses_answers_the_engine_refusals_code() {
    type AnEngineRefusal = fn() -> streamlib::sdk::error::Error;
    let cases: [(AnEngineRefusal, &str); 2] = [
        (
            || streamlib::sdk::error::Error::UnknownNodeType {
                ident: streamlib::sdk::descriptors::NodeClassImportPath::new("nodes.absent:Absent")
                    .unwrap(),
            },
            "node_type_not_found",
        ),
        (
            || streamlib::sdk::error::Error::NodeNameTaken {
                name: "gray".to_owned(),
                cast: "gray".to_owned(),
            },
            "node_name_taken",
        ),
    ];
    for (the_refusal, refusal_code) in cases {
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::refusing_every_add_node_with(the_refusal)),
            "transact",
            json!({
                "mode": "live",
                "operations": [{
                    "operation": "add_node",
                    "stream": STUB_STREAM_ASSET,
                    "type": "nodes.absent:Absent",
                    "name": "gray",
                }],
            }),
        )
        .await;
        let refusal = &body["structuredContent"]["refusal"];
        assert_eq!(refusal["refusal_code"], refusal_code, "{refusal}");
        assert_eq!(refusal["refused_operation_index"], 0, "{refusal}");
    }
}

/// A committed edit is kept in the stream's graph document, so a stream whose directory
/// holds no project is refused before anything is applied.
#[tokio::test]
async fn a_committed_edit_to_a_stream_whose_directory_holds_no_project_is_refused() {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let recorded = runtime.recorded_graph_mutations.clone();

    let body = tool_call_result(
        runtime,
        "add_node",
        json!({ "stream": STUB_STREAM_ASSET, "type": "nodes.fx:Fx" }),
    )
    .await;

    let text = body["content"][0]["text"].as_str().unwrap();
    assert_eq!(body["isError"], true, "{text}");
    assert!(text.contains("holds no project"), "{text}");
    assert!(recorded.lock().is_empty(), "nothing was applied");
}

/// The one setting [`AddNodeTestSourceTakingOneSetting`] takes.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    streamlib::sdk::schemars::JsonSchema,
)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub(super) struct AddNodeTestSourceTakingOneSettingConfig {
    #[serde(default)]
    pub frame_width: Option<u32>,
}

/// A native source whose config refuses every key but `frame_width`.
#[streamlib::sdk::node(
    execution = manual,
    config = crate::mcp::tests::graph_edit_tool_tests::AddNodeTestSourceTakingOneSettingConfig,
    output("video", pin_type = Texture),
)]
pub(super) struct AddNodeTestSourceTakingOneSetting;

impl streamlib::sdk::nodes::ManualNode for AddNodeTestSourceTakingOneSetting::Node {
    fn start(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        Ok(())
    }
}

/// Against a real engine: a live add meets the refusal a load does, naming
/// the node, its type and the setting, and adds nothing.
#[tokio::test(flavor = "multi_thread")]
async fn tools_call_add_node_refuses_a_setting_the_nodes_type_does_not_take_naming_it() {
    streamlib::sdk::nodes::NODE_REGISTRY.register::<AddNodeTestSourceTakingOneSetting::Node>();
    let source_type = AddNodeTestSourceTakingOneSetting::node_class_import_path();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runtime = streamlib::sdk::runtime::Runner::new().unwrap();
    let stream = runtime
        .load_an_empty_stream(
            streamlib::sdk::runtime::OptionsForLoadingOneStream::at_stream_location(
                "main".to_owned(),
                StreamLocation {
                    project_directory: project_directory.path().to_path_buf(),
                    project: uuid::Uuid::new_v4(),
                    stream_asset: STUB_STREAM_ASSET,
                },
            ),
        )
        .unwrap();

    let body = tool_call_result(
        Arc::clone(&runtime) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        "add_node",
        json!({
            "stream": STUB_STREAM_ASSET,
            "type": source_type.as_str(),
            "name": "front",
            "config": {"frame_widht": 640},
            "mode": "live",
        }),
    )
    .await;

    assert_eq!(body["isError"], true, "body={body}");
    let text = body["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("node `front`"), "{text}");
    assert!(text.contains(source_type.as_str()), "{text}");
    assert!(text.contains("`frame_widht`"), "{text}");
    let graph = stream.to_json().unwrap();
    assert_eq!(
        graph["nodes"],
        json!([]),
        "a refused add adds nothing: {graph}"
    );
}

#[tokio::test]
async fn tools_call_add_node_without_config_or_name_sends_an_empty_object_and_answers_the_engines_name()
 {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let recorded = runtime.recorded_graph_mutations.clone();

    let stated = first_text_block_json(
        &tool_call_result(
            runtime,
            "add_node",
            json!({ "stream": STUB_STREAM_ASSET, "type": "tatolab.moq:MoqTrackReceive", "mode": "live" }),
        )
        .await,
    );
    assert_eq!(
        stated["added_nodes"][0]["name"],
        crate::control_plane_stub_support::STUB_ADDED_NODE_NAME,
        "an unnamed node is answered with the name the engine gave it"
    );
    let [streamlib_project_repository::StreamGraphOperation::AddNode { node, .. }] =
        &recorded_stream_graph_operations(&recorded)[..]
    else {
        panic!("expected one add, recorded {:?}", recorded.lock());
    };
    assert!(node.input_values.is_empty());
    assert_eq!(node.name, "");
}

/// Mental-revert: read unknown fields past and `display_name` is dropped,
/// so the node takes the class's short name while the caller believes it
/// named it.
#[tokio::test]
async fn tools_call_add_node_refuses_an_argument_it_does_not_take() {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let recorded = runtime.recorded_graph_mutations.clone();

    let body = tool_call_result(
        runtime,
        "add_node",
        json!({
            "stream": STUB_STREAM_ASSET,
            "type": "nodes.grayscale_effect:GrayscaleEffect",
            "display_name": "gray",
            "mode": "live",
        }),
    )
    .await;

    let text = body["content"][0]["text"].as_str().unwrap();
    assert_eq!(body["isError"], true, "{text}");
    assert!(text.contains("display_name"), "{text}");
    assert!(recorded.lock().is_empty(), "nothing was added");
}

/// Each end is a node's UUID and a port name, handed to the engine as given.
#[tokio::test]
async fn tools_call_connect_and_disconnect_name_each_end_by_its_nodes_uuid() {
    let runtime = a_stub_holding_a_camera_linked_to_an_effect();
    let recorded = runtime.recorded_graph_mutations.clone();
    let link_onto = |target_port: &str| {
        json!({
            "stream": STUB_STREAM_ASSET,
            "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
            "target": { "node": STUB_EFFECT_NODE_UUID, "port": target_port },
            "mode": "live",
        })
    };

    let connected = first_text_block_json(
        &tool_call_result(runtime.clone(), "connect", link_onto("mask")).await,
    );
    let disconnected = first_text_block_json(
        &tool_call_result(runtime, "disconnect", link_onto("video_in")).await,
    );

    let link_of = |target_port: &str| streamlib_project_repository::StreamGraphDocumentLink {
        source: streamlib_project_repository::StreamGraphDocumentLinkEnd {
            node: STUB_CAMERA_NODE_UUID.parse().unwrap(),
            port: "video".to_owned(),
            member: None,
        },
        target: streamlib_project_repository::StreamGraphDocumentLinkEnd {
            node: STUB_EFFECT_NODE_UUID.parse().unwrap(),
            port: target_port.to_owned(),
            member: None,
        },
    };
    assert_eq!(
        recorded_stream_graph_operations(&recorded),
        [
            streamlib_project_repository::StreamGraphOperation::Connect {
                link: link_of("mask")
            },
            streamlib_project_repository::StreamGraphOperation::Disconnect {
                link: link_of("video_in")
            },
        ]
    );
    assert_eq!(
        connected["connected_links"],
        json!([{
            "stream": STUB_STREAM_ID,
            "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
            "target": { "node": STUB_EFFECT_NODE_UUID, "port": "mask" },
        }]),
        "the new link's state is read from the stream's own `graph`"
    );
    assert_eq!(disconnected["connected_links"], json!([]));
}

/// A node is named by its UUID: a name, or a UUID the graph does not hold,
/// is refused naming it before anything reaches the runtime.
#[tokio::test]
async fn a_graph_tool_naming_a_node_by_its_name_or_an_absent_uuid_is_refused_naming_it() {
    let absent_node_uuid = "33333333-3333-4333-8333-333333333333";
    for (tool_name, arguments, what_the_refusal_names) in [
        (
            "remove_node",
            json!({ "stream": STUB_STREAM_ASSET, "node": "fx", "mode": "live" }),
            "`fx` is neither a node's UUID",
        ),
        (
            "connect",
            json!({
                "stream": STUB_STREAM_ASSET,
                "source": { "node": "camera", "port": "video" },
                "target": { "node": STUB_EFFECT_NODE_UUID, "port": "video_in" },
                "mode": "live",
            }),
            "`camera` is neither a node's UUID",
        ),
        (
            "set_node_config",
            json!({ "stream": STUB_STREAM_ASSET, "node": absent_node_uuid, "config": {}, "mode": "live" }),
            absent_node_uuid,
        ),
    ] {
        let runtime = a_stub_holding_a_camera_linked_to_an_effect();
        let recorded = runtime.recorded_graph_mutations.clone();
        let body = tool_call_result(runtime, tool_name, arguments.clone()).await;
        let refused = the_refusal_of_a_tool_result(&body);
        assert_eq!(refused.action, tool_name, "{arguments}");
        let text = refused.runtime_words;
        assert!(text.contains(what_the_refusal_names), "{text}");
        assert!(recorded.lock().is_empty(), "nothing reached the runtime");
    }
}

fn a_studio_at(position: [i64; 2]) -> streamlib_project_repository::StreamGraphNodeStudioFields {
    streamlib_project_repository::StreamGraphNodeStudioFields {
        position: Some(position.map(Into::into)),
        ..Default::default()
    }
}

/// Each edit carries what its inverse needs, read from the graph as it was; a config or a pin
/// value set records one `SetNodeInputValue` for each input value it changes.
#[tokio::test]
async fn each_node_edit_carries_what_the_node_held_before_it() {
    let runtime = a_stub_holding_a_camera_linked_to_an_effect();
    let recorded = runtime.recorded_graph_mutations.clone();
    for (tool_name, arguments) in [
        (
            "set_node_config",
            json!({ "node": STUB_CAMERA_NODE_UUID, "config": { "fps": 60 } }),
        ),
        (
            "set_pin_value",
            json!({ "node": STUB_EFFECT_NODE_UUID, "input": "strength", "value": 0.9 }),
        ),
        (
            "rename_node",
            json!({ "node": STUB_EFFECT_NODE_UUID, "name": "glow" }),
        ),
        (
            "set_studio_fields",
            json!({ "node": STUB_CAMERA_NODE_UUID, "studio": { "position": [4, 2] } }),
        ),
        (
            "set_exposed_config_fields",
            json!({ "node": STUB_CAMERA_NODE_UUID, "exposed_config_fields": ["fps"] }),
        ),
        ("remove_node", json!({ "node": STUB_EFFECT_NODE_UUID })),
    ] {
        let mut arguments = arguments;
        arguments["stream"] = json!(STUB_STREAM_ASSET);
        arguments["mode"] = json!("live");
        first_text_block_json(&tool_call_result(runtime.clone(), tool_name, arguments).await);
    }

    let camera: uuid::Uuid = STUB_CAMERA_NODE_UUID.parse().unwrap();
    let effect: uuid::Uuid = STUB_EFFECT_NODE_UUID.parse().unwrap();
    let recorded = recorded_stream_graph_operations(&recorded);
    use streamlib_project_repository::StreamGraphOperation;
    assert_eq!(
        recorded[0],
        StreamGraphOperation::SetNodeInputValue {
            member: None,
            node: camera,
            input: "fps".to_owned(),
            from: Some(json!(30)),
            to: Some(json!(60)),
        }
    );
    assert_eq!(
        recorded[1],
        StreamGraphOperation::SetNodeInputValue {
            member: None,
            node: effect,
            input: "strength".to_owned(),
            from: Some(json!(0.5)),
            to: Some(json!(0.9)),
        }
    );
    assert_eq!(
        recorded[2],
        StreamGraphOperation::RenameNode {
            node: effect,
            from: "fx".to_owned(),
            to: "glow".to_owned(),
        }
    );
    assert_eq!(
        recorded[3],
        StreamGraphOperation::SetStudioFields {
            node: camera,
            from: Box::new(a_studio_at([0, 0])),
            to: Box::new(a_studio_at([4, 2])),
        }
    );
    assert_eq!(
        recorded[4],
        StreamGraphOperation::SetExposedConfigFields {
            node: camera,
            from: Vec::new(),
            to: vec!["fps".to_owned()],
        }
    );
    let StreamGraphOperation::RemoveNode { node, links } = &recorded[5] else {
        panic!("the last edit removes, recorded {recorded:?}");
    };
    assert_eq!(node.id, effect);
    assert_eq!(
        links.len(),
        1,
        "a removed node carries its links, so undo brings them back: {links:?}"
    );
}

/// `transact` applies its operations in order, a later one naming a node an
/// earlier `add_node` adds by its `as`; one refused takes back the stream's batch, and the
/// take-back undoes the rename before it, which the executor then leaves alone.
#[tokio::test]
async fn transact_names_an_added_node_by_its_as_and_reverses_itself_when_refused() {
    let runtime = a_stub_holding_a_camera_linked_to_an_effect();
    let recorded = runtime.recorded_graph_mutations.clone();

    let stated = first_text_block_json(
        &tool_call_result(
            runtime.clone(),
            "transact",
            json!({
                "mode": "live",
                "operations": [
                    { "operation": "add_node", "stream": STUB_STREAM_ASSET,
                      "type": "nodes.blur:Blur", "as": "blur" },
                    { "operation": "connect", "stream": STUB_STREAM_ASSET,
                      "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
                      "target": { "node": "blur", "port": "video_in" } },
                ],
            }),
        )
        .await,
    );
    let added = stated["added_nodes"][0]["node"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(stated["added_nodes"][0]["as"], "blur");
    assert_eq!(stated["connected_links"][0]["target"]["node"], added);

    recorded.lock().clear();
    let body = tool_call_result(
        runtime,
        "transact",
        json!({
            "mode": "live",
            "operations": [
                { "operation": "rename_node", "stream": STUB_STREAM_ASSET,
                  "node": STUB_EFFECT_NODE_UUID, "name": "glow" },
                { "operation": "remove_node", "stream": STUB_STREAM_ASSET,
                  "node": "33333333-3333-4333-8333-333333333333" },
            ],
        }),
    )
    .await;
    assert_eq!(body["isError"], true, "body={body}");
    use streamlib_project_repository::StreamGraphOperation;
    let effect: uuid::Uuid = STUB_EFFECT_NODE_UUID.parse().unwrap();
    assert_eq!(
        recorded_stream_graph_operations(&recorded),
        [StreamGraphOperation::RenameNode {
            node: effect,
            from: "fx".to_owned(),
            to: "glow".to_owned()
        }],
        "the rename is made in the batch, whose take-back names the node as before, so the \
         executor reverses nothing after it"
    );
}

/// A refused graph edit answers, beside its text, a structured refusal naming the action,
/// the operation it refused and the node or link that operation names, in the runtime's own
/// words as the text carries them.
#[tokio::test]
async fn a_refused_graph_edit_answers_a_structured_refusal_naming_what_it_refused() {
    let absent_node = "33333333-3333-4333-8333-333333333333";
    let body = tool_call_result(
        a_stub_holding_a_camera_linked_to_an_effect(),
        "transact",
        json!({
            "mode": "live",
            "operations": [
                { "operation": "rename_node", "stream": STUB_STREAM_ASSET,
                  "node": STUB_EFFECT_NODE_UUID, "name": "glow" },
                { "operation": "remove_node", "stream": STUB_STREAM_ASSET,
                  "node": absent_node },
            ],
        }),
    )
    .await;
    assert_eq!(body["isError"], true, "body={body}");
    let refusal = &body["structuredContent"]["refusal"];
    assert_eq!(refusal["action"], "transact", "{refusal}");
    assert_eq!(refusal["refused_operation_index"], 1, "{refusal}");
    assert_eq!(refusal["refused_node"], absent_node, "{refusal}");
    assert_eq!(refusal["refusal_code"], "node_not_found", "{refusal}");
    the_refusal_of_a_tool_result(&body);

    let body = tool_call_result(
        a_stub_holding_a_camera_linked_to_an_effect(),
        "connect",
        json!({
            "stream": STUB_STREAM_ASSET, "mode": "live",
            "source": { "node": absent_node, "port": "video" },
            "target": { "node": STUB_EFFECT_NODE_UUID, "port": "video_in" },
        }),
    )
    .await;
    let refusal = &body["structuredContent"]["refusal"];
    assert_eq!(refusal["refused_operation_index"], 0, "{refusal}");
    assert_eq!(
        refusal["refused_link"],
        json!({
            "source": { "node": absent_node, "port": "video" },
            "target": { "node": STUB_EFFECT_NODE_UUID, "port": "video_in" },
        }),
        "{refusal}"
    );
}

/// A blank idempotency key is refused before anything applies.
#[tokio::test]
async fn a_graph_edit_under_a_blank_idempotency_key_is_refused() {
    let runtime = a_stub_holding_a_camera_linked_to_an_effect();
    let recorded = runtime.recorded_graph_mutations.clone();
    let body = tool_call_result(
        runtime,
        "rename_node",
        json!({
            "stream": STUB_STREAM_ASSET, "mode": "live", "node": STUB_EFFECT_NODE_UUID,
            "name": "glow", "idempotency_key": " ",
        }),
    )
    .await;
    assert_eq!(body["isError"], true, "body={body}");
    assert!(
        body["structuredContent"]["refusal"]["runtime_words"]
            .as_str()
            .unwrap()
            .contains("idempotency key"),
        "{body}"
    );
    assert!(recorded.lock().is_empty(), "nothing was applied");
}

#[tokio::test]
async fn a_mutation_tool_with_malformed_arguments_is_a_tool_error_that_reaches_no_op() {
    for (tool_name, arguments, what_the_refusal_names) in [
        (
            "connect",
            json!({ "stream": STUB_STREAM_ASSET, "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" } }),
            "target",
        ),
        (
            "connect",
            json!({
                "stream": STUB_STREAM_ASSET,
                "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
                "target": { "node": STUB_EFFECT_NODE_UUID, "prot": "video_in" },
            }),
            "prot",
        ),
        (
            "disconnect",
            json!({ "stream": STUB_STREAM_ASSET, "link_id": "link-9" }),
            "link_id",
        ),
        (
            "remove_node",
            json!({ "stream": STUB_STREAM_ASSET, "name": "fx" }),
            "name",
        ),
        (
            "transact",
            json!({ "operations": [{ "operation": "split_node" }] }),
            "split_node",
        ),
    ] {
        let runtime = a_stub_holding_a_camera_linked_to_an_effect();
        let recorded = runtime.recorded_graph_mutations.clone();
        let body = tool_call_result(runtime, tool_name, arguments.clone()).await;
        let text = body["content"][0]["text"].as_str().unwrap();
        assert_eq!(body["isError"], true, "{arguments} gave {text}");
        assert!(
            text.contains(what_the_refusal_names),
            "{arguments} gave {text}"
        );
        assert!(
            recorded.lock().is_empty(),
            "a refused call must reach no runtime op"
        );
    }
}
