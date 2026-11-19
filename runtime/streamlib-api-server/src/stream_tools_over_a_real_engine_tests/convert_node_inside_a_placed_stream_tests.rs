// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A convert node a project places by hand inside a placed stream is judged there by the
//! settings it holds, as at the stream's own level.

use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::convert_node_inserted_on_a_link_tests::{
    ConvertNodeTestEnd, MODEL_NODE_UUID, connected_by_alice,
    register_the_convert_node_test_node_types_once, the_links_in,
};
use super::*;

const RGBA_WRITER_NODE_UUID: &str = "e6e6e6e6-e6e6-4e6e-8e6e-e6e6e6e6e6e6";
const BGRA_WRITER_NODE_UUID: &str = "e7e7e7e7-e7e7-4e7e-8e7e-e7e7e7e7e7e7";

/// Inside a placed stream, a `TensorToTexture` placed by hand is judged by the `format` it
/// holds, as at the stream's own level: one holding none writes `rgba8_unorm`, so its link into
/// an input that takes only `bgra8_unorm` is refused by its pin types, `check_link` answering
/// that same refusal, and the placed stream's document is left as it was; one holding
/// `bgra8_unorm` connects there.
///
/// Mental-revert: judging a link inside a placed stream by the ports its node types state, and
/// not by the settings each node holds, connects the first link with a warning that the output
/// states no format.
#[tokio::test(flavor = "multi_thread")]
async fn a_convert_node_inside_a_placed_stream_is_judged_by_the_format_it_holds() {
    register_the_convert_node_test_node_types_once();
    let end_type = ConvertNodeTestEnd::node_class_import_path();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": MODEL_NODE_UUID, "name": "model", "type": end_type.as_str(),
              "config": {}, "studio": { "position": [200.0, 100.0] } },
            { "id": RGBA_WRITER_NODE_UUID, "name": "writes_rgba",
              "type": "tatolab.stream:TensorToTexture", "config": {},
              "studio": { "position": [0.0, 0.0] } },
            { "id": BGRA_WRITER_NODE_UUID, "name": "writes_bgra",
              "type": "tatolab.stream:TensorToTexture", "config": { "format": "bgra8_unorm" },
              "studio": { "position": [0.0, 200.0] } },
        ],
        "links": [],
    }));
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let collapsed = engine
        .the_answer_or_the_refusal_of(
            "collapse_selection_into_subgraph",
            json!({ "stream": stream_asset,
                    "nodes": [MODEL_NODE_UUID, RGBA_WRITER_NODE_UUID, BGRA_WRITER_NODE_UUID],
                    "name": "sub", "author": an_agent_named("alice") }),
        )
        .await
        .expect("the model and both writers collapse into `sub`");
    let subgraph = collapsed["added_nodes"][0]["node"]
        .as_str()
        .expect("the subgraph node")
        .to_owned();
    let drawn = |name: &str| {
        let held = the_node_named(&project.the_graph_document_of("sub"), name);
        the_uuid_a_held_node_takes_under(&subgraph, held["id"].as_str().unwrap())
    };
    let into_the_bgra_input = |writer: &str| {
        json!({ "stream": stream_asset, "level": [subgraph],
                "source": { "node": drawn(writer), "port": "image" },
                "target": { "node": drawn("model"), "port": "bgra_video" } })
    };
    let sub_before = project.the_graph_document_of("sub");

    let checked = engine
        .the_answer_or_the_refusal_of("check_link", into_the_bgra_input("writes_rgba"))
        .await
        .unwrap_or_else(|refusal| panic!("check_link answers: {refusal:?}"));
    assert_eq!(checked["outcome"], "refused", "{checked}");
    assert_eq!(
        checked["refusal"]["refusal_code"], "link_pin_types_do_not_connect",
        "{checked}"
    );
    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&into_the_bgra_input("writes_rgba")),
        )
        .await
        .expect_err("a writer holding no format writes rgba8_unorm, which the input refuses");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert_eq!(
        checked["refusal"],
        serde_json::to_value(&*refusal).unwrap(),
        "check_link answers the refusal connect answers"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before);

    engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&into_the_bgra_input("writes_bgra")),
        )
        .await
        .unwrap_or_else(|refusal| panic!("a writer holding bgra8_unorm connects: {refusal:?}"));
    assert_eq!(
        the_links_in(&project.the_graph_document_of("sub")).len(),
        1,
        "{}",
        project.the_graph_document_of("sub")
    );
}
