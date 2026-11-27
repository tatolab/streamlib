// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Each value input of the built-in nodes written as one function, set through `transact` while
//! its stream plays, reaches the node with no rebuild: the stream keeps running and its graph
//! document equals its running graph.

use streamlib_media_builtins::{BreakBundle, MakeBundle, TensorToAudio, TensorToTexture};

use super::bundle_type_offered_by_its_stream_tests::{
    a_tone, the_stream_describing_three_node_types_of_two_bundles,
};
use super::*;

const TENSOR_TO_AUDIO_UUID: &str = "7a7a7a7a-7a7a-4a7a-8a7a-7a7a7a7a7a7a";
const TENSOR_TO_TEXTURE_UUID: &str = "7b7b7b7b-7b7b-4b7b-8b7b-7b7b7b7b7b7b";
const MAKE_BUNDLE_UUID: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";
const BREAK_BUNDLE_UUID: &str = "7d7d7d7d-7d7d-4d7d-8d7d-7d7d7d7d7d7d";

/// A project whose stream `main` holds one of each built-in that declares a value input, each
/// holding its defaults.
fn a_project_holding_each_built_in_with_a_value_input() -> ProjectWithGraphDocuments {
    streamlib_media_builtins::register_media_builtin_node_types();
    let node = |id: &str, name: &str, node_type: NodeClassImportPath| json!({ "id": id, "name": name, "type": node_type.as_str() });
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            node(TENSOR_TO_AUDIO_UUID, "to_audio", TensorToAudio::node_class_import_path()),
            node(TENSOR_TO_TEXTURE_UUID, "to_texture", TensorToTexture::node_class_import_path()),
            node(MAKE_BUNDLE_UUID, "make", MakeBundle::node_class_import_path()),
            node(BREAK_BUNDLE_UUID, "break", BreakBundle::node_class_import_path()),
        ],
        "links": [],
    }))
}

/// Mental-revert: a built-in whose value input is read at setup is rebuilt by the set and named
/// in `nodes_rebuilt`.
#[tokio::test(flavor = "multi_thread")]
async fn each_value_input_of_a_built_in_set_while_playing_rebuilds_no_node() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_holding_each_built_in_with_a_value_input();
    let (stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    the_stream_describing_three_node_types_of_two_bundles(&engine, &stream_id);

    for (node, input, value) in [
        (TENSOR_TO_AUDIO_UUID, "attribute", json!("audio")),
        (
            TENSOR_TO_AUDIO_UUID,
            "channel_names",
            json!(["left", "right"]),
        ),
        (TENSOR_TO_AUDIO_UUID, "sample_rate", json!(44_100)),
        (TENSOR_TO_TEXTURE_UUID, "attribute", json!("picture")),
        (TENSOR_TO_TEXTURE_UUID, "format", json!("bgra8_unorm")),
        (MAKE_BUNDLE_UUID, "bundle_type", json!(a_tone())),
        (BREAK_BUNDLE_UUID, "bundle_type", json!(a_tone())),
    ] {
        let answer = engine
            .the_answer_or_the_refusal_of(
                "transact",
                json!({ "author": an_agent_named("alice"), "operations": [
                    { "operation": "set_node_input_value",
                      "stream": project.stream_asset_of("main"),
                      "node": node, "input": input, "value": value },
                ] }),
            )
            .await
            .unwrap_or_else(|refusal| panic!("`{input}` of {node} is set: {refusal:?}"));
        assert!(
            answer.get("nodes_rebuilt").is_none(),
            "setting `{input}` of {node} rebuilds no node: {answer}"
        );
    }

    let listing = the_listing_of(&engine.operations_on_the_loaded_streams(), &stream_id).await;
    assert_eq!(listing["state"], "running", "{listing}");
    assert_the_graph_document_equals_the_running_graph(
        &project.the_graph_document_of("main"),
        &engine.the_live_graph_of(&stream_id),
    );
}
