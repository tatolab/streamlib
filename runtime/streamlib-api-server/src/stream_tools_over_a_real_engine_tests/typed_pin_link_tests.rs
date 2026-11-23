// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` over a real engine judges two typed pins by their verdict: a link the verdict
//! refuses is refused with `link_pin_types_do_not_connect`, and a link into a setting with
//! `input_takes_no_wire`, each in the runtime's words and before anything is applied.

use super::input_value_set_tests::{
    MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer, the_input_named,
};
use super::*;

use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

/// The `connect` arguments linking the mixer's output `output` into its input `input`.
fn a_link_from_the_mixer_into_itself(stream_asset: Uuid, output: &str, input: &str) -> Value {
    json!({
        "stream": stream_asset, "author": an_agent_named("alice"),
        "source": { "node": MIXER_NODE_UUID, "port": output },
        "target": { "node": MIXER_NODE_UUID, "port": input },
    })
}

/// A Float output linked into a Texture input is refused, naming both types, and the saved
/// graph is left as it was.
///
/// Mental-revert: judging the two typed pins by their families answers `refused` with the
/// family check's words.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_between_typed_pins_that_do_not_connect_is_refused_with_its_code() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let saved_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            a_link_from_the_mixer_into_itself(stream_asset, "level", "video"),
        )
        .await
        .expect_err("a Float does not connect to a Texture");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert!(
        refusal
            .runtime_words
            .contains("`mixer.level` (Float) to `mixer.video` (Texture)"),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// The UUID of the top-level `GraphInput` `volume` beside the mixer.
const VOLUME_GRAPH_INPUT_NODE_UUID: &str = "70707070-7070-4070-8070-707070707070";

/// A link into the `value` of a top-level `GraphInput`, a setting, is refused as taking no
/// wire, naming the input.
///
/// Mental-revert: refusing the link in words alone answers `refused`.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_into_a_setting_is_refused_as_taking_no_wire() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let mut graph = project.the_graph_document_of("main");
    graph["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": VOLUME_GRAPH_INPUT_NODE_UUID, "name": "volume", "type": "tatolab.stream:GraphInput",
        "input_values": { "port": "volume", "family": "channels" },
    }));
    project.write_the_graph_document(graph);
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            json!({
                "stream": stream_asset, "author": an_agent_named("alice"),
                "source": { "node": MIXER_NODE_UUID, "port": "level" },
                "target": { "node": VOLUME_GRAPH_INPUT_NODE_UUID, "port": "value" },
            }),
        )
        .await
        .expect_err("a setting takes no wire");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputTakesNoWire,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_field.as_deref(),
        Some("value"),
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_node.as_deref(),
        Some(VOLUME_GRAPH_INPUT_NODE_UUID),
        "the node as the caller named it: {refusal:?}"
    );
    assert!(refusal.runtime_words.contains("`volume`"), "{refusal:?}");
}

/// The UUIDs of a top-level `GraphOutput`, `SetVariable` and `GetVariable` beside the mixer.
const LEVEL_GRAPH_OUTPUT_NODE_UUID: &str = "71717171-7171-4171-8171-717171717171";
const GAIN_SET_VARIABLE_NODE_UUID: &str = "72727272-7272-4272-8272-727272727272";
const GAIN_GET_VARIABLE_NODE_UUID: &str = "73737373-7373-4373-8373-737373737373";

/// `graph` renders a setting of a top-level `GraphOutput`, `SetVariable` and `GetVariable` as
/// taking no wire, and a link into one of those settings is refused as taking no wire, naming
/// the setting and the node, as one into a setting of a `GraphInput` is.
///
/// Mental-revert: rendering the settings of a node the graph resolves itself without the rule
/// leaves out `takes_a_wire`; refusing a link into one by its port alone answers `refused`.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_into_a_setting_of_a_node_type_of_the_graph_itself_is_refused_as_taking_no_wire() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let mut graph = project.the_graph_document_of("main");
    let nodes = graph["nodes"].as_array_mut().expect("nodes");
    nodes.push(json!({
        "id": LEVEL_GRAPH_OUTPUT_NODE_UUID, "name": "level-out",
        "type": "tatolab.stream:GraphOutput", "input_values": { "port": "level" },
    }));
    nodes.push(json!({
        "id": GAIN_SET_VARIABLE_NODE_UUID, "name": "gain-set",
        "type": "tatolab.stream:SetVariable", "input_values": { "variable": "gain" },
    }));
    nodes.push(json!({
        "id": GAIN_GET_VARIABLE_NODE_UUID, "name": "gain-get",
        "type": "tatolab.stream:GetVariable", "input_values": { "variable": "gain" },
    }));
    let links = graph["links"].as_array_mut().expect("links");
    for target in ["level-out", "gain-set"] {
        links.push(json!({
            "source": { "node": "mixer", "port": "level" },
            "target": { "node": target, "port": "value" },
        }));
    }
    project.write_the_graph_document(graph);
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let saved_before = project.the_graph_document_of("main");
    let rendered = engine.the_live_graph_of(&stream_id);
    for (node, setting) in [
        ("level-out", "port"),
        ("gain-set", "variable"),
        ("gain-get", "variable"),
    ] {
        let rendered_setting = the_input_named(&the_node_named(&rendered, node), setting);
        assert_eq!(
            rendered_setting["takes_a_wire"],
            json!(false),
            "`graph` renders {node}.{setting}: {rendered_setting}"
        );
    }

    for (node, node_name, setting) in [
        (LEVEL_GRAPH_OUTPUT_NODE_UUID, "level-out", "port"),
        (GAIN_SET_VARIABLE_NODE_UUID, "gain-set", "variable"),
        (GAIN_GET_VARIABLE_NODE_UUID, "gain-get", "variable"),
    ] {
        let refusal = engine
            .the_answer_or_the_refusal_of(
                "connect",
                json!({
                    "stream": stream_asset, "author": an_agent_named("alice"),
                    "source": { "node": MIXER_NODE_UUID, "port": "level" },
                    "target": { "node": node, "port": setting },
                }),
            )
            .await
            .expect_err("a setting of a node type of the graph itself takes no wire");

        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::InputTakesNoWire,
            "{node}.{setting}: {refusal:?}"
        );
        assert_eq!(
            refusal.refused_field.as_deref(),
            Some(setting),
            "{refusal:?}"
        );
        assert_eq!(
            refusal.refused_node.as_deref(),
            Some(node),
            "the node as the caller named it: {refusal:?}"
        );
        assert!(
            refusal
                .runtime_words
                .contains(&format!("input `{setting}` of `{node_name}`")),
            "the runtime's words name the setting and its node: {refusal:?}"
        );
    }
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// A link inside a placed stream is judged by its pin types as the same link at the stream's
/// own level is: a Float into a Texture is refused with `link_pin_types_do_not_connect`, and a
/// link into a setting with `input_takes_no_wire`, each leaving the placed stream's document as
/// it was; a link the verdict connects is saved in it.
///
/// Mental-revert: saving a link inside a placed stream unjudged saves the two refused links.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_inside_a_placed_stream_is_judged_by_its_pin_types() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let inside = engine
        .the_mixer_collapsed_into_a_placed_stream(&project)
        .await;
    let link_inside = |source: (&str, &str), target: (&str, &str)| {
        json!({
            "stream": stream_asset, "level": [inside.subgraph], "author": an_agent_named("alice"),
            "source": { "node": source.0, "port": source.1 },
            "target": { "node": target.0, "port": target.1 },
        })
    };
    let sub_before = project.the_graph_document_of("sub");

    let pin_types_refused = engine
        .the_answer_or_the_refusal_of(
            "connect",
            link_inside((&inside.mixer, "level"), (&inside.mixer, "video")),
        )
        .await
        .expect_err("a Float does not connect to a Texture inside a placed stream");
    let setting_refused = engine
        .the_answer_or_the_refusal_of(
            "connect",
            link_inside((&inside.mixer, "level"), (&inside.volume, "value")),
        )
        .await
        .expect_err("a setting inside a placed stream takes no wire");

    assert_eq!(
        pin_types_refused.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{pin_types_refused:?}"
    );
    assert!(
        pin_types_refused
            .runtime_words
            .contains("`mixer.level` (Float) to `mixer.video` (Texture)"),
        "{pin_types_refused:?}"
    );
    assert_eq!(
        setting_refused.refusal_code,
        ControlOperationRefusalCode::InputTakesNoWire,
        "{setting_refused:?}"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before);

    engine
        .the_answer_or_the_refusal_of(
            "connect",
            link_inside((&inside.mixer, "level"), (&inside.mixer, "gain")),
        )
        .await
        .expect("a Float into a Float connects inside a placed stream");
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("sub")),
        ["mixer/level -> mixer/gain"]
    );
}

/// A link inside a placed stream reaching a `GraphOutput` that names no family, which runs as
/// no node of its own, is saved without being judged, and its entry under `connected_links`
/// says why; the same `connect` sent again under its key says why again.
///
/// Mental-revert: discarding what the judgement of a link at a placed level answers leaves the
/// entry with no `not_judged_because`; answering the retry without the reasons the transaction
/// keeps leaves the retry's entry with none.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_inside_a_placed_stream_saved_unjudged_says_why() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let inside = engine
        .the_mixer_collapsed_into_a_placed_stream(&project)
        .await;
    let added = engine
        .the_answer_or_the_refusal_of(
            "add_node",
            json!({ "stream": stream_asset, "level": [inside.subgraph],
                    "type": "tatolab.stream:GraphOutput", "name": "level-out",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("a `GraphOutput` naming no family is added inside `sub`");
    let output = added["added_nodes"][0]["node"]
        .as_str()
        .expect("the output's UUID")
        .to_owned();

    let connect = json!({
        "stream": stream_asset, "level": [inside.subgraph],
        "author": an_agent_named("alice"), "idempotency_key": "level-out",
        "source": { "node": inside.mixer, "port": "level" },
        "target": { "node": output, "port": "value" },
    });
    let connected = engine
        .the_answer_or_the_refusal_of("connect", connect.clone())
        .await
        .expect("a link into a `GraphOutput` naming no family is saved");

    let why = connected["connected_links"][0]["not_judged_because"]
        .as_str()
        .unwrap_or_else(|| panic!("the link says why it was not judged: {connected}"));
    assert!(why.contains("runs as no node of its own"), "{why}");
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("sub")),
        ["mixer/level -> level-out/value"]
    );

    let sent_again = engine
        .the_answer_or_the_refusal_of("connect", connect)
        .await
        .unwrap_or_else(|refusal| panic!("the connect sent again answers: {refusal:?}"));
    assert_eq!(
        sent_again["transaction"], connected["transaction"],
        "{sent_again}"
    );
    assert_eq!(
        sent_again["connected_links"][0]["not_judged_because"].as_str(),
        Some(why),
        "{sent_again}"
    );
}
