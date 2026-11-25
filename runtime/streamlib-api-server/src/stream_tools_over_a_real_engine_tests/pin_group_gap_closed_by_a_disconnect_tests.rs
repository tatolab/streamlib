// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A disconnect of a middle slot of a pin group following its wires, over a real engine: the
//! later slot's link moves down into the gap in the same transaction, in the saved document and
//! in the stream's graph, the node keeps one empty slot after it, and one undo puts every link
//! back in its slot.

use streamlib::sdk::descriptors::{NodeClassImportPath, NodeClassShortName, PinDeclaration, PinGroupDescriptor, PinType, PortDescriptor};

use super::*;

const SOURCE_TYPE: &str = "pin_group_gap_test:Source";
const MIXER_TYPE: &str = "pin_group_gap_test:Mixer";
const MIXER_UUID: &str = "4d4d4d4d-4d4d-4d4d-8d4d-4d4d4d4d4d4d";
const SOURCE_UUIDS: [&str; 3] = [
    "5e5e5e5e-5e5e-4e5e-8e5e-5e5e5e5e5e50",
    "5e5e5e5e-5e5e-4e5e-8e5e-5e5e5e5e5e51",
    "5e5e5e5e-5e5e-4e5e-8e5e-5e5e5e5e5e52",
];

fn a_float_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(name, "", true, PinDeclaration::exactly(PinType::Float { exact_size: None }))
    .with_delivery_profile("newest")
}

fn a_node_type(node_type: &str) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type,
        "a fixture",
    )
}

fn the_pin_group_gap_node_types_registered() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        for descriptor in [
            a_node_type(SOURCE_TYPE).with_output(a_float_port("value")),
            a_node_type(MIXER_TYPE).with_pin_group(PinGroupDescriptor::new(
                "layers",
                "each layer mixed",
                vec![a_float_port("layer")],
                Vec::new(),
            )),
        ] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("each pin group gap node type is registered here alone");
        }
    });
}

/// Three sources, each linked into one slot of the mixer's `layers`.
fn a_project_whose_mixer_has_three_linked_slots() -> ProjectWithGraphDocuments {
    the_pin_group_gap_node_types_registered();
    let mut nodes = vec![json!({ "id": MIXER_UUID, "name": "mixer", "type": MIXER_TYPE })];
    let mut links = Vec::new();
    for (slot, source) in SOURCE_UUIDS.iter().enumerate() {
        nodes.push(json!({ "id": source, "name": format!("s{slot}"), "type": SOURCE_TYPE }));
        links.push(json!({
            "source": { "node": source, "port": "value" },
            "target": { "node": MIXER_UUID, "port": format!("layers.{slot}.layer") },
        }));
    }
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": nodes, "links": links,
    }))
}

/// The names of the inputs of the mixer in `graph`.
fn the_mixers_inputs_in(graph: &Value) -> Vec<String> {
    graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["id"] == MIXER_UUID)
        .map(|mixer| {
            mixer["ports"]["inputs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|input| input["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Mental-revert: without the gap closed, `s2` stays linked into slot 2 past the empty slot 1.
#[tokio::test(flavor = "multi_thread")]
async fn a_disconnect_of_a_middle_slot_moves_the_later_link_down_and_one_undo_puts_it_back() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_mixer_has_three_linked_slots();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let as_loaded = [
        "s0/value -> mixer/layers.0.layer",
        "s1/value -> mixer/layers.1.layer",
        "s2/value -> mixer/layers.2.layer",
    ];
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        as_loaded
    );

    let applied = engine
        .the_answer_or_the_refusal_of(
            "disconnect",
            json!({
                "stream": stream,
                "source": { "node": SOURCE_UUIDS[1], "port": "value" },
                "target": { "node": MIXER_UUID, "port": "layers.1.layer" },
                "author": an_agent_named("alice"),
            }),
        )
        .await
        .expect("the disconnect applies");

    let closed = [
        "s0/value -> mixer/layers.0.layer",
        "s2/value -> mixer/layers.1.layer",
    ];
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main")),
        closed
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(links_by_node_name_in(&live), closed, "{live}");
    assert_eq!(
        the_mixers_inputs_in(&live),
        ["layers.0.layer", "layers.1.layer", "layers.2.layer"],
        "one empty slot after the last linked one"
    );

    let undone = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the undo applies");
    assert_eq!(undone["undoes"], applied["transaction"], "{undone}");
    let sorted = |mut links: Vec<String>| {
        links.sort();
        links
    };
    assert_eq!(
        sorted(links_by_node_name_in(
            &project.the_graph_document_of("main")
        )),
        as_loaded,
        "each link is back in its slot"
    );
    assert_eq!(
        sorted(links_by_node_name_in(&engine.the_live_graph_of(&stream_id))),
        as_loaded
    );
}
