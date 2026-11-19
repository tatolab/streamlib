// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link whose two pin types connect only through a built-in conversion gets that convert node,
//! collapsed, in the same edit as the `connect`, named in its answer, and one `undo` removes the
//! node and both links; `check_link` answers the node `connect` would place. A link only a
//! module's conversion bridges is refused naming it, with the `transact` that places it.

use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, SuggestedOperationCall,
};

use super::*;

/// A native node with a typed output and input of each type a convert node bridges, and a Float
/// output and Integer input a free conversion joins; two of it face each other in a stream.
#[streamlib::sdk::node(
    execution = manual,
    input("tensor", delivery_profile = "newest", pin_type = Tensor),
    input("bgra_video", delivery_profile = "newest", pin_type = Texture, formats = ["bgra8_unorm"]),
    input("rgba_video", delivery_profile = "newest", pin_type = Texture, formats = ["rgba8_unorm"]),
    input("nv12_video", delivery_profile = "newest", pin_type = Texture, formats = ["nv12"]),
    input("count", delivery_profile = "newest", pin_type = Integer),
    input("audio", delivery_profile = "newest", pin_type = Audio),
    output("video", pin_type = Texture, formats = ["rgba8_unorm"]),
    output("tensor_out", pin_type = Tensor),
    output("gate", pin_type = Boolean),
    output("level", pin_type = Float),
)]
pub(super) struct ConvertNodeTestEnd;

impl ManualNode for ConvertNodeTestEnd::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// A conversion a module states, of a Boolean into Audio, which `connect` never places.
#[streamlib::sdk::node(
    execution = manual,
    input("gate", delivery_profile = "newest", pin_type = Boolean),
    output("audio", pin_type = Audio),
)]
#[meta(
    conversion = true,
    description = "Sounds a tone while the gate is open."
)]
pub(super) struct ConvertNodeTestModuleGateToAudio;

impl ManualNode for ConvertNodeTestModuleGateToAudio::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// Register the test ends, the module's conversion and the built-in conversions, once per test
/// binary.
fn register_the_convert_node_test_node_types_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        NODE_REGISTRY.register::<ConvertNodeTestEnd::Node>();
        NODE_REGISTRY.register::<ConvertNodeTestModuleGateToAudio::Node>();
        streamlib_media_builtins::register_the_conversion_node_types();
    });
}

const CAMERA_NODE_UUID: &str = "c4c4c4c4-c4c4-4c4c-8c4c-c4c4c4c4c4c4";
const MODEL_NODE_UUID: &str = "d5d5d5d5-d5d5-4d5d-8d5d-d5d5d5d5d5d5";

/// A project whose stream `main` holds two [`ConvertNodeTestEnd`]s, `camera` drawn at (0, 0) and
/// `model` drawn at (200, 100), and no link.
fn a_project_whose_main_stream_holds_a_camera_and_a_model() -> ProjectWithGraphDocuments {
    register_the_convert_node_test_node_types_once();
    let node_type = ConvertNodeTestEnd::node_class_import_path();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": CAMERA_NODE_UUID, "name": "camera", "type": node_type.as_str(),
              "config": {}, "studio": { "position": [0.0, 0.0] } },
            { "id": MODEL_NODE_UUID, "name": "model", "type": node_type.as_str(),
              "config": {}, "studio": { "position": [200.0, 100.0] } },
        ],
        "links": [],
    }))
}

/// The link from `camera`'s `output` to `model`'s `input` in `stream`, as `check_link` takes it.
fn camera_into_model(stream: Uuid, output: &str, input: &str) -> Value {
    json!({
        "stream": stream,
        "source": { "node": CAMERA_NODE_UUID, "port": output },
        "target": { "node": MODEL_NODE_UUID, "port": input },
    })
}

/// `link` as `connect` takes it, by the agent `alice`.
fn connected_by_alice(link: &Value) -> Value {
    let mut connect = link.clone();
    connect["author"] = an_agent_named("alice");
    connect
}

/// The node of `graph_document` other than the camera and the model.
fn the_inserted_node_in(graph_document: &Value) -> Value {
    let mut others = graph_document["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["id"] != CAMERA_NODE_UUID && node["id"] != MODEL_NODE_UUID);
    let inserted = others.next().cloned().expect("a node was inserted");
    assert!(others.next().is_none(), "{graph_document}");
    inserted
}

/// Each link of `graph_document` as `(source node, source port, target node, target port)`.
fn the_links_in(graph_document: &Value) -> Vec<(String, String, String, String)> {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    graph_document["links"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|link| {
            (
                text(&link["source"]["node"]),
                text(&link["source"]["port"]),
                text(&link["target"]["node"]),
                text(&link["target"]["port"]),
            )
        })
        .collect()
}

fn link(source: (&str, &str), target: (&str, &str)) -> (String, String, String, String) {
    (
        source.0.to_owned(),
        source.1.to_owned(),
        target.0.to_owned(),
        target.1.to_owned(),
    )
}

/// A Texture linked into a Tensor gets a collapsed `TextureToTensor` and both its links in one
/// edit, drawn halfway between the two nodes; the answer names the node, its type and why it
/// was inserted, with both links, and the same `connect` sent again under its key answers the
/// same; and one `undo` removes the node and both links, from the document and the running
/// graph.
///
/// Mental-revert: refusing the link, as the engine's own connect does, answers
/// `link_pin_types_do_not_connect` and adds nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_a_built_in_conversion_bridges_gets_its_convert_node_in_one_edit() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");
    let live_before = engine.the_live_graph_of(&stream_id);

    let mut connect = connected_by_alice(&camera_into_model(
        project.stream_asset_of("main"),
        "video",
        "tensor",
    ));
    connect["idempotency_key"] = json!("convert-gesture");
    let applied = engine
        .the_answer_or_the_refusal_of("connect", connect.clone())
        .await
        .unwrap_or_else(|refusal| panic!("the link connects through a node: {refusal:?}"));

    let saved = project.the_graph_document_of("main");
    let inserted = the_inserted_node_in(&saved);
    let inserted_uuid = inserted["id"].as_str().expect("a UUID").to_owned();
    assert_eq!(
        inserted["type"], "tatolab.stream:TextureToTensor",
        "{saved}"
    );
    assert_eq!(inserted["studio"]["collapsed"], true, "{saved}");
    assert_eq!(
        inserted["studio"]["position"],
        json!([100.0, 50.0]),
        "{saved}"
    );
    assert_eq!(
        the_links_in(&saved),
        [
            link((CAMERA_NODE_UUID, "video"), (&inserted_uuid, "image")),
            link((&inserted_uuid, "array"), (MODEL_NODE_UUID, "tensor")),
        ]
    );
    let added_nodes = applied["added_nodes"].as_array().expect("added_nodes");
    assert_eq!(added_nodes.len(), 1, "{applied}");
    assert_eq!(added_nodes[0]["node"], inserted_uuid.as_str(), "{applied}");
    assert_eq!(
        added_nodes[0]["node_type"], "tatolab.stream:TextureToTensor",
        "{applied}"
    );
    assert_eq!(
        added_nodes[0]["inserted_because"], "converts_pin_types",
        "{applied}"
    );
    let connected_links = applied["connected_links"]
        .as_array()
        .expect("connected_links");
    assert_eq!(connected_links.len(), 2, "{applied}");
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)).len(),
        3,
        "the running graph holds the convert node"
    );
    let sent_again = engine
        .the_answer_or_the_refusal_of("connect", connect)
        .await
        .unwrap_or_else(|refusal| panic!("the connect sent again answers: {refusal:?}"));
    assert_eq!(
        sent_again["transaction"], applied["transaction"],
        "{sent_again}"
    );
    assert_eq!(
        sent_again["added_nodes"], applied["added_nodes"],
        "{sent_again}"
    );

    let undone = engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the undo applies");
    assert_eq!(undone["undoes"], applied["transaction"], "{undone}");
    let saved_after = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&saved_after),
        sorted_node_names_in(&saved_before)
    );
    assert_eq!(the_links_in(&saved_after), [], "{saved_after}");
    let live_after = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        sorted_node_names_in(&live_after),
        sorted_node_names_in(&live_before)
    );
    assert_eq!(
        links_by_node_name_in(&live_after),
        Vec::<String>::new(),
        "{live_after}"
    );
}

/// A Tensor linked into a Texture input that takes `bgra8_unorm` gets a `TensorToTexture`
/// holding `format` `bgra8_unorm`, the format it picks for that input; into an input that takes
/// only `nv12`, which it never writes, the link is refused and nothing is added.
///
/// A second link out of that node, into an input that takes only `rgba8_unorm`, is refused by
/// its pin types: the node writes the `bgra8_unorm` it holds.
///
/// Mental-revert: a conversion judged by its output as it states itself, a Texture of no format,
/// is placed into the `nv12` input with a warning, and the node into `bgra8_unorm` holds no
/// `format`; a link out of the node judged by that same declared output warns that it states no
/// format, and the second link connects with that warning.
#[tokio::test(flavor = "multi_thread")]
async fn a_convert_node_holds_the_setting_it_picks_for_the_input_and_none_is_placed_without_one() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&camera_into_model(stream_asset, "tensor_out", "nv12_video")),
        )
        .await
        .expect_err("no format TensorToTexture writes is one the input takes");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert_eq!(the_links_in(&project.the_graph_document_of("main")), []);

    let applied = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&camera_into_model(stream_asset, "tensor_out", "bgra_video")),
        )
        .await
        .unwrap_or_else(|refusal| panic!("the link connects through a node: {refusal:?}"));
    let connected_links = applied["connected_links"]
        .as_array()
        .expect("connected_links");
    assert_eq!(connected_links.len(), 2, "{applied}");
    assert_eq!(
        connected_links[1]["target"]["port"], "bgra_video",
        "{applied}"
    );
    assert!(
        connected_links[1]["state"].is_string(),
        "the link out of the node is read from the graph: {applied}"
    );
    assert!(
        connected_links[1].get("warnings").is_none(),
        "the node's output is judged by the format it holds: {applied}"
    );
    let inserted = the_inserted_node_in(&project.the_graph_document_of("main"));
    assert_eq!(
        inserted["type"], "tatolab.stream:TensorToTexture",
        "{inserted}"
    );
    assert_eq!(
        inserted["input_values"],
        json!({ "format": "bgra8_unorm" }),
        "{inserted}"
    );

    let saved_before = project.the_graph_document_of("main");
    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&json!({
                "stream": stream_asset,
                "source": { "node": inserted["id"], "port": "image" },
                "target": { "node": MODEL_NODE_UUID, "port": "rgba_video" },
            })),
        )
        .await
        .expect_err("the node writes bgra8_unorm, which the input does not take");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// `check_link` answers `connects_through_convert_node` with the node `connect` would place, its
/// ports, their types and the settings it would hold, and changes nothing.
///
/// Mental-revert: answering the refusal the engine's own connect gives the link reports
/// `refused` for a link `connect` makes.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_the_convert_node_connect_would_place() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");

    let checked = engine
        .the_answer_or_the_refusal_of(
            "check_link",
            camera_into_model(project.stream_asset_of("main"), "tensor_out", "bgra_video"),
        )
        .await
        .unwrap_or_else(|refusal| panic!("check_link answers: {refusal:?}"));

    assert_eq!(
        checked,
        json!({
            "outcome": "connects_through_convert_node",
            "convert_node": {
                "node_type": "tatolab.stream:TensorToTexture",
                "input_port": "array",
                "input_pin_type": { "type": "tensor" },
                "output_port": "image",
                "output_pin_type": { "type": "texture", "pixel_formats": ["bgra8_unorm"] },
                "input_values": { "format": "bgra8_unorm" },
            },
        })
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// Each link a transaction connects says how its input reads the output for free.
///
/// Mental-revert: an answer that never judges its links states no `free_conversion`.
#[tokio::test(flavor = "multi_thread")]
async fn a_connected_link_says_when_it_converts_for_free() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let applied = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&camera_into_model(
                project.stream_asset_of("main"),
                "level",
                "count",
            )),
        )
        .await
        .unwrap_or_else(|refusal| panic!("a Float is read as an Integer: {refusal:?}"));

    assert_eq!(
        applied["connected_links"][0]["free_conversion"], true,
        "{applied}"
    );
    assert!(applied["added_nodes"].as_array().is_some_and(Vec::is_empty));
}

/// A link only a module's conversion bridges is refused with `link_needs_a_module_conversion`,
/// naming that node type, and its `suggested_call` is the `transact` that places it between the
/// two ends: made, it adds the node and both links.
///
/// `check_link` answers that same refusal and call.
///
/// Mental-revert: coding the refusal as any link whose pin types do not connect answers
/// `link_pin_types_do_not_connect` and suggests nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_only_a_module_conversion_bridges_is_refused_with_the_call_that_places_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let module_conversion = ConvertNodeTestModuleGateToAudio::node_class_import_path();

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&camera_into_model(
                project.stream_asset_of("main"),
                "gate",
                "audio",
            )),
        )
        .await
        .expect_err("connect never places a module's conversion");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkNeedsAModuleConversion,
        "{refusal:?}"
    );
    assert!(
        refusal
            .runtime_words
            .contains(&format!("`{module_conversion}`")),
        "{refusal:?}"
    );
    let Some(SuggestedOperationCall { operation, params }) = refusal.suggested_call.clone() else {
        panic!("the refusal suggests the call that places the conversion: {refusal:?}");
    };
    assert_eq!(operation, "transact");
    assert_eq!(
        Value::Object(params.clone()),
        json!({ "operations": [
            { "operation": "add_node", "stream": stream_id,
              "type": module_conversion.as_str(), "as": "conversion" },
            { "operation": "connect", "stream": stream_id,
              "source": { "node": CAMERA_NODE_UUID, "port": "gate" },
              "target": { "node": "conversion", "port": "gate" } },
            { "operation": "connect", "stream": stream_id,
              "source": { "node": "conversion", "port": "audio" },
              "target": { "node": MODEL_NODE_UUID, "port": "audio" } },
        ]})
    );

    let checked = engine
        .the_answer_or_the_refusal_of(
            "check_link",
            camera_into_model(project.stream_asset_of("main"), "gate", "audio"),
        )
        .await
        .unwrap_or_else(|refusal| panic!("check_link answers: {refusal:?}"));
    assert_eq!(checked["outcome"], "refused", "{checked}");
    assert_eq!(
        checked["refusal"]["refusal_code"], "link_needs_a_module_conversion",
        "{checked}"
    );
    assert_eq!(
        checked["refusal"]["suggested_call"],
        serde_json::to_value(&refusal.suggested_call).unwrap(),
        "check_link suggests the call connect suggests"
    );

    let mut suggested = Value::Object(params);
    suggested["author"] = an_agent_named("alice");
    let placed = engine
        .the_answer_or_the_refusal_of("transact", suggested)
        .await
        .unwrap_or_else(|refusal| panic!("the suggested call places it: {refusal:?}"));
    assert_eq!(placed["added_nodes"].as_array().map(Vec::len), Some(1));
    assert_eq!(placed["connected_links"].as_array().map(Vec::len), Some(2));
}

/// A refused link one end of which is a node the same `transact` adds, named by its `as`, gets no
/// `suggested_call`: the stream holds no such node for a later call to name.
///
/// Mental-revert: suggesting the call whatever the ends name offers a `transact` naming
/// `conversion`, a node no stream holds, beside the label it gives the conversion it places.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_from_a_node_the_same_transact_adds_is_refused_suggesting_no_call() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_id,
                  "type": ConvertNodeTestEnd::node_class_import_path().as_str(),
                  "as": "conversion" },
                { "operation": "connect", "stream": stream_id,
                  "source": { "node": "conversion", "port": "gate" },
                  "target": { "node": MODEL_NODE_UUID, "port": "audio" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("connect never places a module's conversion");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkNeedsAModuleConversion,
        "{refusal:?}"
    );
    assert_eq!(refusal.suggested_call, None, "{refusal:?}");
}

const SPARE_NODE_UUID: &str = "e6e6e6e6-e6e6-4e6e-8e6e-e6e6e6e6e6e6";

/// A link needing a convert node on a stream an earlier operation of the same transaction has
/// take its edits once the documents hold them, here a collapse placing a stream, is refused
/// before anything is written, naming the node and the transaction of its own that places it;
/// so is one only a module's conversion bridges, `link_needs_a_module_conversion`. A Float into
/// an Integer connects there.
///
/// Mental-revert: connecting either refused link as it is writes it to the document, and the
/// running stream only warns of it as it takes the edit.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_needing_a_convert_node_on_a_stream_taking_its_edits_later_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    let mut graph = project.the_graph_document_of("main");
    graph["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": SPARE_NODE_UUID, "name": "spare",
        "type": ConvertNodeTestEnd::node_class_import_path().as_str(), "config": {},
    }));
    project.write_the_graph_document(graph);
    let stream_asset = project.stream_asset_of("main");
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [
                { "operation": "collapse_selection_into_subgraph", "stream": stream_asset,
                  "nodes": [SPARE_NODE_UUID], "name": "sub" },
                { "operation": "connect", "stream": stream_asset,
                  "source": { "node": CAMERA_NODE_UUID, "port": "tensor_out" },
                  "target": { "node": MODEL_NODE_UUID, "port": "bgra_video" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("no convert node is placed on a stream taking its edits later");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert_eq!(refusal.refused_operation_index, Some(1), "{refusal:?}");
    assert!(
        refusal
            .runtime_words
            .contains("`tatolab.stream:TensorToTexture`")
            && refusal
                .runtime_words
                .contains("in a transaction of its own"),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);

    let collapsed_then_connected = |output: &str, input: &str| {
        json!({ "operations": [
            { "operation": "collapse_selection_into_subgraph", "stream": stream_asset,
              "nodes": [SPARE_NODE_UUID], "name": "sub" },
            { "operation": "connect", "stream": stream_asset,
              "source": { "node": CAMERA_NODE_UUID, "port": output },
              "target": { "node": MODEL_NODE_UUID, "port": input } },
        ], "author": an_agent_named("alice") })
    };
    let refusal = engine
        .the_answer_or_the_refusal_of("transact", collapsed_then_connected("gate", "audio"))
        .await
        .expect_err("only a module's conversion bridges a Boolean into Audio");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkNeedsAModuleConversion,
        "{refusal:?}"
    );
    assert_eq!(refusal.refused_operation_index, Some(1), "{refusal:?}");
    assert_eq!(project.the_graph_document_of("main"), saved_before);

    engine
        .the_answer_or_the_refusal_of("transact", collapsed_then_connected("level", "count"))
        .await
        .unwrap_or_else(|refusal| panic!("a Float connects into an Integer: {refusal:?}"));
    assert!(
        the_links_in(&project.the_graph_document_of("main")).contains(&link(
            (CAMERA_NODE_UUID, "level"),
            (MODEL_NODE_UUID, "count")
        )),
        "{}",
        project.the_graph_document_of("main")
    );
}

/// A Tensor linked into a Texture input that takes `bgra8_unorm` inside a placed stream, where
/// `connect` places no convert node, is refused before anything is written, naming the node to
/// add there; a Boolean linked into Audio, which only a module's conversion bridges, is refused
/// `link_needs_a_module_conversion` with the `transact` that places it at that level, as at the
/// stream's own level; a Tensor into an input that takes only `nv12`, which no conversion
/// bridges, is refused by its pin types; neither graph document changes. A Float into an Integer
/// connects there.
///
/// Mental-revert: connecting any of the three refused links as it is writes it to the placed
/// stream's document.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_needing_a_convert_node_inside_a_placed_stream_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let collapsed = engine
        .the_answer_or_the_refusal_of(
            "collapse_selection_into_subgraph",
            json!({ "stream": stream_asset, "nodes": [CAMERA_NODE_UUID, MODEL_NODE_UUID],
                    "name": "sub", "author": an_agent_named("alice") }),
        )
        .await
        .expect("the camera and the model collapse into `sub`");
    let subgraph = collapsed["added_nodes"][0]["node"]
        .as_str()
        .expect("the subgraph node")
        .to_owned();
    let drawn = |name: &str| {
        let held = the_node_named(&project.the_graph_document_of("sub"), name);
        the_uuid_a_held_node_takes_under(&subgraph, held["id"].as_str().unwrap())
    };
    let (camera_drawn, model_drawn) = (drawn("camera"), drawn("model"));
    let main_before = project.the_graph_document_of("main");
    let sub_before = project.the_graph_document_of("sub");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            json!({ "stream": stream_asset, "level": [subgraph],
                    "source": { "node": camera_drawn, "port": "tensor_out" },
                    "target": { "node": model_drawn, "port": "bgra_video" },
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("no convert node is placed inside a placed stream");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert!(
        refusal
            .runtime_words
            .contains("`tatolab.stream:TensorToTexture`")
            && refusal.runtime_words.contains("inside a placed stream"),
        "{refusal:?}"
    );

    let module_conversion = ConvertNodeTestModuleGateToAudio::node_class_import_path();
    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            json!({ "stream": stream_asset, "level": [subgraph],
                    "source": { "node": camera_drawn, "port": "gate" },
                    "target": { "node": model_drawn, "port": "audio" },
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("connect never places a module's conversion");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkNeedsAModuleConversion,
        "{refusal:?}"
    );
    let Some(SuggestedOperationCall { operation, params }) = refusal.suggested_call.clone() else {
        panic!("the refusal suggests the call that places the conversion: {refusal:?}");
    };
    assert_eq!(operation, "transact");
    assert_eq!(
        Value::Object(params),
        json!({ "operations": [
            { "operation": "add_node", "stream": stream_id, "level": [subgraph],
              "type": module_conversion.as_str(), "as": "conversion" },
            { "operation": "connect", "stream": stream_id, "level": [subgraph],
              "source": { "node": camera_drawn, "port": "gate" },
              "target": { "node": "conversion", "port": "gate" } },
            { "operation": "connect", "stream": stream_id, "level": [subgraph],
              "source": { "node": "conversion", "port": "audio" },
              "target": { "node": model_drawn, "port": "audio" } },
        ]})
    );
    let connected_inside = |output: &str, input: &str| {
        json!({ "stream": stream_asset, "level": [subgraph],
                "source": { "node": camera_drawn, "port": output },
                "target": { "node": model_drawn, "port": input },
                "author": an_agent_named("alice") })
    };
    let refusal = engine
        .the_answer_or_the_refusal_of("connect", connected_inside("tensor_out", "nv12_video"))
        .await
        .expect_err("no conversion writes nv12 from a Tensor");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before);
    assert_eq!(project.the_graph_document_of("main"), main_before);

    engine
        .the_answer_or_the_refusal_of("connect", connected_inside("level", "count"))
        .await
        .unwrap_or_else(|refusal| panic!("a Float connects into an Integer: {refusal:?}"));
    assert_eq!(
        the_links_in(&project.the_graph_document_of("sub")).len(),
        1,
        "{}",
        project.the_graph_document_of("sub")
    );
}

const TENSOR_ISLAND_TYPE: &str = "convert_node_test:TensorIsland";
const TENSOR_ISLAND_UUID: &str = "f7f7f7f7-f7f7-4f7f-8f7f-f7f7f7f7f7f7";

/// Register a function node type giving a Tensor on `array`, once per test binary.
fn register_a_function_node_giving_a_tensor_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        use streamlib::sdk::descriptors::{
            FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeKind,
            PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
        };
        let tensor = PinType::Tensor {};
        let mut descriptor = NodeDescriptor::new(
            NodeClassShortName::new("TensorIsland").expect("a short name"),
            NodeClassImportPath::new(TENSOR_ISLAND_TYPE).expect("a node type"),
            "a function node giving a Tensor",
        )
        .with_output(
            PortDescriptor::new(
                "array",
                "",
                true,
                PortFamilyDeclaration::of_family(
                    PortFamily::carrying(&tensor).unwrap_or(PortFamily::Records),
                ),
            )
            .with_pin_declaration(PinDeclaration::exactly(tensor)),
        );
        descriptor.node_kind = NodeKind::Function {
            language: FunctionNodeLanguage::Python,
        };
        NODE_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("the function node type is registered here alone");
    });
}

/// A function node's Tensor linked into a Texture input would run through a `TensorToTexture`
/// node, which runs in the app process that runs no node code, so the island would feed it:
/// `connect` refuses the link, and `check_link` answers `refused` with that same refusal rather
/// than the convert node.
///
/// Mental-revert: a check answering the convert node without the island checks of the two links
/// through it answers `connects_through_convert_node` for a link `connect` refuses.
#[tokio::test(flavor = "multi_thread")]
async fn a_convert_node_an_island_would_feed_is_refused_by_check_link_as_connect_refuses_it() {
    register_a_function_node_giving_a_tensor_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    let mut graph = project.the_graph_document_of("main");
    graph["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": TENSOR_ISLAND_UUID, "name": "island", "type": TENSOR_ISLAND_TYPE, "config": {},
    }));
    project.write_the_graph_document(graph);
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");
    let island_into_model = json!({
        "stream": project.stream_asset_of("main"),
        "source": { "node": TENSOR_ISLAND_UUID, "port": "array" },
        "target": { "node": MODEL_NODE_UUID, "port": "bgra_video" },
    });

    let checked = engine
        .the_answer_or_the_refusal_of("check_link", island_into_model.clone())
        .await
        .unwrap_or_else(|refusal| panic!("check_link answers: {refusal:?}"));
    let refused = engine
        .the_answer_or_the_refusal_of("connect", connected_by_alice(&island_into_model))
        .await
        .expect_err("the app process runs no function node");

    assert_eq!(checked["outcome"], "refused", "{checked}");
    assert_eq!(
        checked["refusal"]["refusal_code"],
        serde_json::to_value(refused.refusal_code).unwrap(),
        "{checked}"
    );
    assert!(
        refused
            .runtime_words
            .contains("feeds a function node island")
            && checked["refusal"]["runtime_words"]
                .as_str()
                .is_some_and(|words| words.contains("feeds a function node island")),
        "{checked} {refused:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}
