// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use crate::core::descriptors::{NamedPinTypeImportPath, NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType, PortDescriptor};
use crate::core::graph::graph_snapshot::{
    GraphSnapshot, GraphSnapshotWithGraphOnlyNodesResolved, NodeChangeADrawnLinkBlocks,
};
use crate::core::json_schema::{GraphNodeOutput, GraphNodePortsOnTheWire, GraphResponse};
use crate::core::nodes::{HowANodeOfADescribedTypeRuns, NodeTypesOneStreamResolves};
use crate::core::{Error, Result};

const RELAY: &str = "connection_wire_tests.nodes:Relay";
const TRACK: &str = "connection_wire_tests.nodes:Track";
const SEND: &str = "connection_wire_tests.nodes:Send";
const RELAY_CONNECTION: &str = "connection_wire_tests.types:RelayConnection";
const TRACK_CONNECTION: &str = "connection_wire_tests.types:TrackConnection";

fn a_connection_port(name: &str, connection_type: &str, sole_referrer: bool) -> PortDescriptor {
    PortDescriptor::new(name, name, false, PinDeclaration::exactly(PinType::Connection {
        connection_type: NamedPinTypeImportPath::new(connection_type).unwrap(),
        one_referrer_per_referenced_node: sole_referrer,
    }))
}

/// A relay giving itself out as a `RelayConnection`; a track taking one on `relay` and giving
/// itself out as a `TrackConnection`; a send taking a track on `track`, one send at most to a
/// track.
fn node_types_of_a_relay_its_tracks_and_their_sends() -> NodeTypesOneStreamResolves {
    let node_types = NodeTypesOneStreamResolves::new();
    let a_node_type = |short_name: &str, import_path: &str| {
        NodeDescriptor::new(
            NodeClassShortName::new(short_name).unwrap(),
            NodeClassImportPath::new(import_path).unwrap(),
            "connection wire test",
        )
    };
    for descriptor in [
        a_node_type("Relay", RELAY).with_output(a_connection_port(
            "connection",
            RELAY_CONNECTION,
            false,
        )),
        a_node_type("Track", TRACK)
            .with_input(a_connection_port("relay", RELAY_CONNECTION, false))
            .with_output(a_connection_port("connection", TRACK_CONNECTION, false)),
        a_node_type("Send", SEND).with_input(a_connection_port("track", TRACK_CONNECTION, true)),
    ] {
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor,
                HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|_node| {
                    Err(Error::NotSupported("never constructed".into()))
                })),
            )
            .expect("the type is described into the stream");
    }
    node_types
}

fn resolved(graph: serde_json::Value) -> Result<GraphSnapshotWithGraphOnlyNodesResolved> {
    let node_types = node_types_of_a_relay_its_tracks_and_their_sends();
    let resolution = GraphSnapshot::from_graph_document(graph)
        .expect("the graph parses")
        .with_graph_only_nodes_resolved(&node_types)?;
    resolution.validate(&node_types)?;
    Ok(resolution)
}

/// `graph` as it renders: the nodes the runtime runs, with no running link, and what the graph
/// drew rendered back into it.
fn rendered(resolution: &GraphSnapshotWithGraphOnlyNodesResolved) -> GraphResponse {
    let mut graph_response = GraphResponse {
        asset: None,
        display_name: None,
        stream: None,
        clock: Default::default(),
        condition: None,
        nodes: resolution
            .graph_the_runtime_runs
            .nodes
            .iter()
            .map(|node| GraphNodeOutput {
                id: format!("id-{}", node.name),
                node_type: node.node_type.clone(),
                kind: None,
                language: None,
                name: node.name.clone(),
                ports: GraphNodePortsOnTheWire {
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                },
                components: Default::default(),
                input_values: node.input_values.clone(),
                live: None,
                studio: serde_json::Map::new(),
                inputs_waiting_for_a_value: Vec::new(),
                fragment: None,
            })
            .collect(),
        links: Vec::new(),
        references: Vec::new(),
        machine: "rig".to_string(),
        groups: Vec::new(),
        studio: None,
        function_node_pool: None,
    };
    resolution
        .graph_only_nodes_as_drawn
        .render_into(&mut graph_response);
    graph_response
}

/// Each rendered link as `source/port -> target/port`.
fn the_links_of(graph_response: &GraphResponse) -> Vec<String> {
    graph_response
        .links
        .iter()
        .map(|link| {
            format!(
                "{}/{} -> {}/{}",
                link.source.node, link.source.port, link.target.node, link.target.port
            )
        })
        .collect()
}

fn a_wire(source: (&str, &str), target: (&str, &str)) -> serde_json::Value {
    json!({
        "source": {"node": source.0, "port": source.1},
        "target": {"node": target.0, "port": target.1}
    })
}

/// Mental-revert: without the lowering the wire stays a link of the graph that runs, and its
/// track holds no reference to set up after.
#[test]
fn a_wire_into_a_connection_input_runs_as_the_reference_it_brings_and_renders_as_drawn() {
    let resolution = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "camera-track", "type": TRACK}
        ],
        "links": [a_wire(("cloudflare", "connection"), ("camera-track", "relay"))]
    }))
    .expect("a relay wired into its track resolves");

    let runs = &resolution.graph_the_runtime_runs;
    assert!(runs.links.is_empty(), "{:?}", runs.links);
    assert_eq!(
        runs.nodes[1].input_values["relay"],
        json!({"node": "cloudflare"})
    );
    let wires = &resolution
        .graph_only_nodes_as_drawn
        .connection_wires_as_drawn
        .wires;
    assert_eq!(wires.len(), 1);
    assert_eq!(
        (wires[0].source.node.as_str(), wires[0].target.port.as_str()),
        ("cloudflare", "relay")
    );
    let render = rendered(&resolution);
    assert_eq!(
        the_links_of(&render),
        ["cloudflare/connection -> camera-track/relay"]
    );
    assert!(!render.nodes[1].input_values.contains_key("relay"));
}

#[test]
fn a_reference_set_into_a_variable_and_got_reaches_the_same_node() {
    let resolution = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "set-relay", "type": "tatolab.stream:SetVariable", "config": {"variable": "relay"}},
            {"name": "get-relay", "type": "tatolab.stream:GetVariable", "config": {"variable": "relay"}},
            {"name": "camera-track", "type": TRACK}
        ],
        "links": [
            a_wire(("cloudflare", "connection"), ("set-relay", "value")),
            a_wire(("get-relay", "value"), ("camera-track", "relay"))
        ]
    }))
    .expect("a relay passed through a variable resolves");

    let runs = &resolution.graph_the_runtime_runs;
    assert!(runs.links.is_empty(), "{:?}", runs.links);
    let track = runs
        .nodes
        .iter()
        .find(|node| node.name == "camera-track")
        .expect("the track runs");
    assert_eq!(track.input_values["relay"], json!({"node": "cloudflare"}));
    let mut links = the_links_of(&rendered(&resolution));
    links.sort();
    assert_eq!(
        links,
        [
            "cloudflare/connection -> set-relay/value",
            "get-relay/value -> camera-track/relay"
        ]
    );
}

/// Mental-revert: with the connection wires left out of the drawn links a change checks, a
/// renamed relay leaves its track holding a name no node has.
#[test]
fn a_node_a_connection_wire_names_is_kept_from_a_rename_naming_the_wire() {
    let resolution = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "camera-track", "type": TRACK}
        ],
        "links": [a_wire(("cloudflare", "connection"), ("camera-track", "relay"))]
    }))
    .expect("a relay wired into its track resolves");

    for node in ["cloudflare", "camera-track"] {
        let refusal = resolution
            .graph_only_nodes_as_drawn
            .refuse_a_node_change_a_drawn_link_names(node, NodeChangeADrawnLinkBlocks::Rename)
            .expect_err("a wired node keeps its name")
            .to_string();
        assert!(
            refusal.contains(
                "wires `cloudflare/connection` into the connection input `camera-track/relay`"
            ),
            "{refusal}"
        );
    }
}

#[test]
fn a_connection_of_one_type_is_refused_on_an_input_of_another_naming_both() {
    let refusal = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "camera-send", "type": SEND}
        ],
        "links": [a_wire(("cloudflare", "connection"), ("camera-send", "track"))]
    }))
    .expect_err("a relay is no track")
    .to_string();

    assert!(
        refusal.contains(RELAY_CONNECTION) && refusal.contains(TRACK_CONNECTION),
        "{refusal}"
    );
}

/// Mental-revert: with a Connection input left out of the inputs that hold a reference, a
/// second send on one track is taken.
#[test]
fn a_second_wire_from_a_node_into_an_input_taking_one_referrer_is_refused_with_todays_reason() {
    let refusal = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "camera-track", "type": TRACK},
            {"name": "camera-send", "type": SEND},
            {"name": "second-send", "type": SEND}
        ],
        "links": [
            a_wire(("cloudflare", "connection"), ("camera-track", "relay")),
            a_wire(("camera-track", "connection"), ("camera-send", "track")),
            a_wire(("camera-track", "connection"), ("second-send", "track"))
        ]
    }))
    .expect_err("one send at most writes a track")
    .to_string();

    assert!(
        refusal.contains("names node `camera-track`")
            && refusal.contains("node `camera-send` already names under `track`"),
        "{refusal}"
    );
}

#[test]
fn a_second_wire_into_one_connection_input_is_refused_naming_both() {
    let refusal = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "fastly", "type": RELAY},
            {"name": "camera-track", "type": TRACK}
        ],
        "links": [
            a_wire(("cloudflare", "connection"), ("camera-track", "relay")),
            a_wire(("fastly", "connection"), ("camera-track", "relay"))
        ]
    }))
    .expect_err("a track takes one relay")
    .to_string();

    assert!(
        refusal.contains("wired from both `cloudflare/connection` and `fastly/connection`"),
        "{refusal}"
    );
}

#[test]
fn a_wire_from_an_output_its_node_does_not_have_is_refused_listing_its_outputs() {
    let refusal = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "camera-track", "type": TRACK}
        ],
        "links": [a_wire(("cloudflare", "session"), ("camera-track", "relay"))]
    }))
    .expect_err("a relay has no `session`")
    .to_string();

    assert!(
        refusal.contains("node `cloudflare` has no output port `session`")
            && refusal.contains("connection"),
        "{refusal}"
    );
}

/// Mental-revert: with no check of a member, the wire lowers to the whole node and the member
/// it names is dropped unsaid.
#[test]
fn a_connection_wire_naming_a_member_is_refused() {
    let refusal = resolved(json!({
        "nodes": [
            {"name": "cloudflare", "type": RELAY},
            {"name": "camera-track", "type": TRACK}
        ],
        "links": [{
            "source": {"node": "cloudflare", "port": "connection", "member": "session"},
            "target": {"node": "camera-track", "port": "relay"}
        }]
    }))
    .expect_err("a connection has no members")
    .to_string();

    assert!(refusal.contains("naming member `session`"), "{refusal}");
}
