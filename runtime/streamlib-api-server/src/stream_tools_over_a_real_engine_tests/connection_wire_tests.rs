// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A wire from a node's connection output into another node's connection input, over a real
//! engine: the stream runs it as the reference it brings, `graph` renders the wire as drawn and
//! the reference beside it, and a live disconnect and connect of it take the reference out and
//! put it back.

use streamlib::sdk::descriptors::{NamedPinTypeImportPath, NodeClassImportPath, NodeClassShortName, PinDeclaration, PinType, PortDescriptor};

use super::*;

const RELAY_TYPE: &str = "connection_wire_test:Relay";
const TRACK_TYPE: &str = "connection_wire_test:Track";
const RELAY_UUID: &str = "6c6c6c6c-6c6c-4c6c-8c6c-6c6c6c6c6c60";
const TRACK_UUID: &str = "6c6c6c6c-6c6c-4c6c-8c6c-6c6c6c6c6c61";
const THE_WIRE: &str = "cloudflare/connection -> camera-track/relay";

fn a_relay_connection_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(name, "", false, PinDeclaration::exactly(PinType::Connection {
        connection_type: NamedPinTypeImportPath::new("connection_wire_test:RelayConnection")
            .expect("a connection type"),
        one_referrer_per_referenced_node: false,
    }))
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

fn the_connection_wire_node_types_registered() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        for descriptor in [
            a_node_type(RELAY_TYPE).with_output(a_relay_connection_port("connection")),
            a_node_type(TRACK_TYPE).with_input(a_relay_connection_port("relay")),
        ] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("each connection wire node type is registered here alone");
        }
    });
}

fn a_project_whose_track_is_wired_to_its_relay() -> ProjectWithGraphDocuments {
    the_connection_wire_node_types_registered();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": RELAY_UUID, "name": "cloudflare", "type": RELAY_TYPE },
            { "id": TRACK_UUID, "name": "camera-track", "type": TRACK_TYPE },
        ],
        "links": [{
            "source": { "node": RELAY_UUID, "port": "connection" },
            "target": { "node": TRACK_UUID, "port": "relay" },
        }],
    }))
}

/// The references `graph` renders, each as `node -> referenced node`.
fn the_references_in(graph: &Value) -> Vec<String> {
    graph["references"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|reference| {
            format!(
                "{} -> {}",
                reference["node"].as_str().unwrap_or_default(),
                reference["referenced_node"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// Mental-revert: without an edit of a wire into a connection input drawn first, the disconnect
/// is refused as naming a link the running graph does not hold.
#[tokio::test(flavor = "multi_thread")]
async fn a_connection_wire_runs_as_its_reference_and_a_live_disconnect_and_connect_follow_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_track_is_wired_to_its_relay();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(links_by_node_name_in(&live), [THE_WIRE], "{live}");
    assert_eq!(
        the_references_in(&live),
        ["camera-track -> cloudflare"],
        "{live}"
    );

    let the_wire = json!({
        "stream": stream,
        "source": { "node": RELAY_UUID, "port": "connection" },
        "target": { "node": TRACK_UUID, "port": "relay" },
        "author": an_agent_named("alice"),
    });
    engine
        .the_answer_or_the_refusal_of("disconnect", the_wire.clone())
        .await
        .expect("the disconnect applies");
    assert!(links_by_node_name_in(&project.the_graph_document_of("main")).is_empty());
    let live = engine.the_live_graph_of(&stream_id);
    assert!(links_by_node_name_in(&live).is_empty(), "{live}");
    assert!(the_references_in(&live).is_empty(), "{live}");

    engine
        .the_answer_or_the_refusal_of("connect", the_wire)
        .await
        .expect("the connect applies");
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main")),
        [THE_WIRE]
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(links_by_node_name_in(&live), [THE_WIRE], "{live}");
    assert_eq!(
        the_references_in(&live),
        ["camera-track -> cloudflare"],
        "{live}"
    );
}
