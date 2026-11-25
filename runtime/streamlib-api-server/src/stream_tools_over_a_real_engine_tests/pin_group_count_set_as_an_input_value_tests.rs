// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An output pin group's count, over a real engine: `set_node_input_value` on the input named
//! after the group saves the count in the graph document, gives the node that many slots in the
//! stream's graph, and one undo takes both back.

use streamlib::sdk::descriptors::{
    NodeClassImportPath, NodeClassShortName, PinDeclaration, PinGroupDescriptor, PinType,
    PortDescriptor,
};

use super::input_value_set_tests::the_input_values_of;
use super::*;

const TAPS_TYPE: &str = "pin_group_count_test:Taps";
const TAPS_UUID: &str = "7a7a7a7a-7a7a-4a7a-8a7a-7a7a7a7a7a7a";

fn the_pin_group_count_node_type_registered() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        let node_type = NodeClassImportPath::new(TAPS_TYPE).expect("a node type");
        let descriptor = NodeDescriptor::new(
            NodeClassShortName::new("Taps").expect("a short name"),
            node_type,
            "a fixture",
        )
        .with_pin_group(PinGroupDescriptor::new(
            "taps",
            "each tap",
            Vec::new(),
            vec![PortDescriptor::new(
                "tap",
                "",
                true,
                PinDeclaration::exactly(PinType::Float { exact_size: None }),
            )],
        ));
        NODE_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("the pin group count node type is registered here alone");
    });
}

/// A project whose stream `main` holds one `taps` node with its count left at its default.
fn a_project_holding_a_taps_node() -> ProjectWithGraphDocuments {
    the_pin_group_count_node_type_registered();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [{ "id": TAPS_UUID, "name": "taps", "type": TAPS_TYPE }],
        "links": [],
    }))
}

/// The names of the outputs of the `taps` node in `graph`.
fn the_taps_outputs_in(graph: &Value) -> Vec<String> {
    the_node_named(graph, "taps")["ports"]["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|output| output["name"].as_str().map(str::to_owned))
        .collect()
}

/// Mental-revert: a count not read back from the saved value leaves the node at one slot.
#[tokio::test(flavor = "multi_thread")]
async fn a_count_set_with_set_node_input_value_is_saved_grows_the_group_and_one_undo_takes_it_back()
{
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_holding_a_taps_node();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    assert_eq!(
        the_taps_outputs_in(&engine.the_live_graph_of(&stream_id)),
        ["taps.0.tap"],
        "the count's default holds one slot"
    );

    let applied = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": TAPS_UUID,
                    "input": "taps", "value": 3, "author": an_agent_named("alice") }),
        )
        .await
        .expect("the count is set");

    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), TAPS_UUID),
        json!({ "taps": 3 }),
        "the count is saved"
    );
    assert_eq!(
        the_taps_outputs_in(&engine.the_live_graph_of(&stream_id)),
        ["taps.0.tap", "taps.1.tap", "taps.2.tap"]
    );

    let undone = engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the undo applies");
    assert_eq!(undone["undoes"], applied["transaction"], "{undone}");
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), TAPS_UUID),
        json!({}),
        "one undo takes the saved count off"
    );
    assert_eq!(
        the_taps_outputs_in(&engine.the_live_graph_of(&stream_id)),
        ["taps.0.tap"],
        "and gives the slots back"
    );
}
