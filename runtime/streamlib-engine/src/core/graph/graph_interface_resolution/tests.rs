// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::PortFamilyDeclaration;
use streamlib_runtime_client_contract::local_api_wire_contract::InputValueComesFromOnTheWire;

fn the_family_no_linked_port_states(
    _linked_port: &LinkPortRefOutput,
    _direction: GraphInterfacePortDirection,
) -> Option<PortFamily> {
    None
}

fn resolved(graph: serde_json::Value) -> Result<GraphSnapshotWithInterfaceResolved> {
    GraphSnapshot::from_graph_document(graph).and_then(|graph| {
        graph.with_graph_interface_resolved(
            GraphInterfaceLevel::AFragment,
            &the_family_no_linked_port_states,
        )
    })
}

fn refusal_of(graph: serde_json::Value) -> String {
    resolved(graph)
        .expect_err("the graph is refused")
        .to_string()
}

fn a_camera_offered_as_an_output_and_a_display_fed_by_an_input() -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            {"name": "camera", "type": "tatolab.camera:CameraSource"},
            {"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
             "input_values": {"port": "video", "family": "image"}},
            {"name": "overlay", "type": GRAPH_INPUT_NODE_TYPE,
             "input_values": {"port": "overlay", "family": "image"}},
            {"name": "display", "type": "tatolab.display:Display"},
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"},
             "target": {"node": "video", "port": "value"}},
            {"source": {"node": "overlay", "port": "value"},
             "target": {"node": "display", "port": "video"}},
        ],
    })
}

#[test]
fn interface_nodes_leave_the_graph_the_runtime_runs_and_their_ports_are_kept_beside_it() {
    let resolution = resolved(a_camera_offered_as_an_output_and_a_display_fed_by_an_input())
        .expect("the interface resolves");
    let names: Vec<&str> = resolution
        .graph_the_runtime_runs
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(names, ["camera", "display"]);
    assert!(resolution.graph_the_runtime_runs.links.is_empty());
    let ports: Vec<&GraphInterfacePort> = resolution.interface_as_drawn.declared_ports().collect();
    assert_eq!(ports.len(), 2);
    assert_eq!(ports[0].direction, GraphInterfacePortDirection::Output);
    assert_eq!(ports[0].port, "video");
    assert_eq!(ports[0].family, Some(PortFamily::Image));
    assert_eq!(ports[0].linked_ports[0].node, "camera");
    assert_eq!(ports[1].direction, GraphInterfacePortDirection::Input);
    assert_eq!(ports[1].linked_ports[0].node, "display");
}

fn resolved_on_the_streams_top_level(
    graph: serde_json::Value,
) -> Result<GraphSnapshotWithInterfaceResolved> {
    GraphSnapshot::from_graph_document(graph).and_then(|graph| {
        graph.with_graph_interface_resolved(
            GraphInterfaceLevel::TheStreamsTopLevel,
            &the_family_no_linked_port_states,
        )
    })
}

fn a_records_input_read_by_two_nodes(input_config: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            {"name": "cue", "type": GRAPH_INPUT_NODE_TYPE, "input_values": input_config},
            {"name": "left", "type": "tatolab.test:Reader"},
            {"name": "right", "type": "tatolab.test:Reader"},
        ],
        "links": [
            {"source": {"node": "cue", "port": "value"},
             "target": {"node": "left", "port": "cue"}},
            {"source": {"node": "cue", "port": "value"},
             "target": {"node": "right", "port": "cue"}},
        ],
    })
}

#[test]
fn a_top_level_input_a_client_writes_runs_with_its_links_and_keeps_its_declared_port() {
    let resolution = resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "family": "records", "default": {"track": "intro"}}),
    ))
    .expect("the interface resolves");

    let names: Vec<&str> = resolution
        .graph_the_runtime_runs
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(names, ["cue", "left", "right"]);
    assert_eq!(resolution.graph_the_runtime_runs.links.len(), 2);
    assert!(
        resolution
            .interface_as_drawn
            .links_touching_an_interface_node
            .is_empty()
    );
    let declared: Vec<&GraphInterfacePort> =
        resolution.interface_as_drawn.declared_ports().collect();
    assert!(declared[0].runs_as_a_source_node);
    assert_eq!(declared[0].linked_ports.len(), 2);
}

#[test]
fn a_top_level_image_input_and_every_fragment_input_run_nothing() {
    let top_level = resolved_on_the_streams_top_level(
        a_camera_offered_as_an_output_and_a_display_fed_by_an_input(),
    )
    .expect("the interface resolves");
    let names: Vec<&str> = top_level
        .graph_the_runtime_runs
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(names, ["camera", "display"]);

    let in_a_fragment = resolved(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "family": "records"}),
    ))
    .expect("the interface resolves");
    assert_eq!(in_a_fragment.graph_the_runtime_runs.nodes.len(), 2);
    assert!(in_a_fragment.graph_the_runtime_runs.links.is_empty());
}

#[test]
fn a_default_or_range_is_refused_where_no_client_writes_the_input_or_its_family_cannot_take_it() {
    let in_a_fragment = refusal_of(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "family": "records", "default": {}}),
    ));
    assert!(
        in_a_fragment.contains("`default`") && in_a_fragment.contains("top level"),
        "{in_a_fragment}"
    );
    let refusal_on_the_top_level = |input_config: serde_json::Value| {
        resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(input_config))
            .expect_err("the input is refused")
            .to_string()
    };
    let default_of_the_wrong_shape = refusal_on_the_top_level(
        serde_json::json!({"port": "cue", "family": "records", "default": 3}),
    );
    assert!(
        default_of_the_wrong_shape.contains("`cue`")
            && default_of_the_wrong_shape.contains("JSON object"),
        "{default_of_the_wrong_shape}"
    );
    let range_on_records = refusal_on_the_top_level(
        serde_json::json!({"port": "cue", "family": "records", "range": [0, 1]}),
    );
    assert!(
        range_on_records.contains("Array or a Channels"),
        "{range_on_records}"
    );
    let range_highest_first = refusal_on_the_top_level(
        serde_json::json!({"port": "cue", "family": "array", "range": [1, 0]}),
    );
    assert!(
        range_highest_first.contains("lowest first"),
        "{range_highest_first}"
    );
    resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "family": "channels", "default": true,
                               "range": [0, 1]}),
    ))
    .expect("a Channels Input takes a bool default and a range");
}

#[test]
fn an_interface_node_naming_no_family_and_linked_to_nothing_runs_nothing() {
    let resolution = resolved_on_the_streams_top_level(serde_json::json!({
        "nodes": [
            {"name": "input1", "type": GRAPH_INPUT_NODE_TYPE, "input_values": {"port": "input1"}},
            {"name": "output1", "type": GRAPH_OUTPUT_NODE_TYPE,
             "input_values": {"port": "output1"}},
        ],
        "links": [],
    }))
    .expect("an interface node waiting for a link resolves");
    assert!(resolution.graph_the_runtime_runs.nodes.is_empty());
    let declared: Vec<&GraphInterfacePort> =
        resolution.interface_as_drawn.declared_ports().collect();
    assert_eq!(declared.len(), 2);
    assert!(declared.iter().all(|port| port.family.is_none()));
    assert!(declared.iter().all(|port| !port.runs_as_a_source_node));
    let rendered = resolution.interface_as_drawn.interface_nodes[0].rendered_as_a_node();
    assert_eq!(rendered.ports.outputs[0].family, None);
    assert_eq!(
        rendered
            .components
            .graph_interface
            .map(|graph_interface| graph_interface.family),
        Some(None)
    );
}

#[test]
fn an_interface_node_naming_no_family_takes_the_family_of_the_first_port_linked_to_it() {
    let the_reader_takes_channels_and_the_camera_writes_image =
        |linked_port: &LinkPortRefOutput, direction: GraphInterfacePortDirection| match (
            linked_port.node.as_str(),
            direction,
        ) {
            ("left", GraphInterfacePortDirection::Input) => Some(PortFamily::Channels),
            ("camera", GraphInterfacePortDirection::Output) => Some(PortFamily::Image),
            _ => None,
        };
    let mut graph = a_records_input_read_by_two_nodes(serde_json::json!({"port": "cue"}));
    graph["nodes"]
        .as_array_mut()
        .expect("the graph has nodes")
        .extend([
            serde_json::json!({"name": "camera", "type": "tatolab.camera:CameraSource"}),
            serde_json::json!({"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
                                   "input_values": {"port": "video"}}),
        ]);
    graph["links"]
        .as_array_mut()
        .expect("the graph has links")
        .push(
            serde_json::json!({"source": {"node": "camera", "port": "video"},
                                     "target": {"node": "video", "port": "value"}}),
        );
    let resolution = GraphSnapshot::from_graph_document(graph)
        .and_then(|graph| {
            graph.with_graph_interface_resolved(
                GraphInterfaceLevel::TheStreamsTopLevel,
                &the_reader_takes_channels_and_the_camera_writes_image,
            )
        })
        .expect("each interface node resolves its family from its first link");

    let declared: Vec<&GraphInterfacePort> =
        resolution.interface_as_drawn.declared_ports().collect();
    assert_eq!(declared[0].family, Some(PortFamily::Channels));
    assert!(declared[0].runs_as_a_source_node);
    assert_eq!(declared[1].family, Some(PortFamily::Image));
    let cue_as_it_runs = resolution
        .graph_the_runtime_runs
        .nodes
        .iter()
        .find(|node| node.name == "cue")
        .expect("a top-level Channels input runs");
    assert_eq!(
        serde_json::Value::Object(cue_as_it_runs.input_values.clone()),
        serde_json::json!({"port": "cue", "family": "channels"})
    );
    assert_eq!(
        serde_json::Value::Object(
            resolution.interface_as_drawn.interface_nodes[0]
                .node
                .input_values
                .clone()
        ),
        serde_json::json!({"port": "cue"}),
        "the node renders back as drawn"
    );
}

#[test]
fn an_output_following_an_input_fed_by_a_graph_input_naming_no_family_carries_that_inputs_family() {
    use crate::core::descriptors::{
        NodeClassImportPath, NodeClassShortName, NodeDescriptor, PortDescriptor,
    };
    use crate::core::nodes::NODE_REGISTRY;

    const PASSER_TYPE: &str = "interface_resolution_fixtures.follows_its_input:Passer";
    const READER_TYPE: &str = "interface_resolution_fixtures.reads_channels:Reader";
    let register = |node_type: &str, inputs: Vec<PortDescriptor>, outputs: Vec<PortDescriptor>| {
        let class_name = node_type
            .rsplit(':')
            .next()
            .expect("the type names a class");
        let mut descriptor = NodeDescriptor::new(
            NodeClassShortName::new(class_name).expect("the class name is short"),
            NodeClassImportPath::new(node_type).expect("the type is an import path"),
            class_name,
        );
        descriptor.inputs = inputs;
        descriptor.outputs = outputs;
        NODE_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("the fixture type registers");
    };
    register(
        PASSER_TYPE,
        vec![PortDescriptor::new(
            "value",
            "input",
            true,
            PortFamilyDeclaration {
                also_takes_families: vec![PortFamily::Channels],
                ..PortFamilyDeclaration::of_family(PortFamily::Image)
            },
        )],
        vec![PortDescriptor::new(
            "result",
            "output",
            true,
            PortFamilyDeclaration {
                family_follows_input_port: Some("value".into()),
                ..PortFamilyDeclaration::of_family(PortFamily::Image)
            },
        )],
    );
    register(
        READER_TYPE,
        vec![PortDescriptor::new(
            "signals",
            "input",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Channels),
        )],
        Vec::new(),
    );
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [
            {"name": "cue", "type": GRAPH_INPUT_NODE_TYPE, "input_values": {"port": "cue"}},
            {"name": "reader", "type": READER_TYPE},
            {"name": "passer", "type": PASSER_TYPE},
            {"name": "out", "type": GRAPH_OUTPUT_NODE_TYPE, "input_values": {"port": "out"}},
        ],
        "links": [
            {"source": {"node": "cue", "port": "value"},
             "target": {"node": "reader", "port": "signals"}},
            {"source": {"node": "cue", "port": "value"},
             "target": {"node": "passer", "port": "value"}},
            {"source": {"node": "passer", "port": "result"},
             "target": {"node": "out", "port": "value"}},
        ],
    }))
    .expect("the graph is a graph document");

    let family_into_out = the_family_a_port_linked_to_an_interface_node_carries(
        &NodeTypesOneStreamResolves::new(),
        &graph,
        &LinkPortRefOutput {
            node: "passer".into(),
            port: "result".into(),
            member: None,
        },
        GraphInterfacePortDirection::Output,
    );

    assert_eq!(family_into_out, Some(PortFamily::Channels));
}

#[test]
fn an_interface_node_naming_no_family_linked_to_a_port_stating_none_is_refused() {
    let refusal = refusal_of(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue"}),
    ));
    assert!(
        refusal.contains("`cue` names no family") && refusal.contains("`left/cue`"),
        "{refusal}"
    );
}

#[test]
fn a_graph_with_no_interface_node_resolves_to_itself() {
    let graph = serde_json::json!({
        "nodes": [{"name": "camera", "type": "tatolab.camera:CameraSource"}],
        "links": [],
    });
    let resolution = resolved(graph.clone()).expect("resolves");
    assert_eq!(
        resolution.graph_the_runtime_runs,
        GraphSnapshot::from_graph_document(graph).expect("parses")
    );
}

#[test]
fn two_outputs_declaring_one_port_are_refused_naming_both() {
    let refusal = refusal_of(serde_json::json!({
        "nodes": [
            {"name": "camera", "type": "tatolab.camera:CameraSource"},
            {"name": "first", "type": GRAPH_OUTPUT_NODE_TYPE,
             "input_values": {"port": "Video", "family": "image"}},
            {"name": "second", "type": GRAPH_OUTPUT_NODE_TYPE,
             "input_values": {"port": "video", "family": "image"}},
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"},
             "target": {"node": "first", "port": "value"}},
            {"source": {"node": "camera", "port": "video"},
             "target": {"node": "second", "port": "value"}},
        ],
    }));
    assert!(
        refusal.contains("`first`") && refusal.contains("`second`") && refusal.contains("`video`"),
        "got {refusal}"
    );
}

#[test]
fn an_input_and_an_output_may_share_a_port_name() {
    let resolution = resolved(serde_json::json!({
        "nodes": [
            {"name": "effect", "type": "my_app:Effect"},
            {"name": "video-in", "type": GRAPH_INPUT_NODE_TYPE,
             "input_values": {"port": "video", "family": "image"}},
            {"name": "video-out", "type": GRAPH_OUTPUT_NODE_TYPE,
             "input_values": {"port": "video", "family": "image"}},
        ],
        "links": [
            {"source": {"node": "video-in", "port": "value"},
             "target": {"node": "effect", "port": "video_in"}},
            {"source": {"node": "effect", "port": "video_out"},
             "target": {"node": "video-out", "port": "value"}},
        ],
    }))
    .expect("an Input and an Output of one name resolve");
    assert_eq!(resolution.interface_as_drawn.declared_ports().count(), 2);
}

#[test]
fn an_output_linked_from_no_port_is_refused() {
    let unlinked = refusal_of(serde_json::json!({
        "nodes": [{"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
                   "input_values": {"port": "video", "family": "image"}}],
        "links": [],
    }));
    assert!(unlinked.contains("from 0 links"), "got {unlinked}");
}

#[test]
fn a_link_into_an_input_or_out_of_an_output_is_refused_naming_the_node() {
    let mut into_an_input = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
    into_an_input["links"]
        .as_array_mut()
        .expect("links")
        .push(serde_json::json!({
            "source": {"node": "camera", "port": "video"},
            "target": {"node": "overlay", "port": "value"},
        }));
    let refusal = refusal_of(into_an_input);
    assert!(
        refusal.contains("enters input `value` of `overlay`, a setting, which takes no wire"),
        "got {refusal}"
    );

    let mut out_of_an_output = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
    out_of_an_output["links"]
        .as_array_mut()
        .expect("links")
        .push(serde_json::json!({
            "source": {"node": "video", "port": "value"},
            "target": {"node": "display", "port": "video"},
        }));
    let refusal = refusal_of(out_of_an_output);
    assert!(
        refusal.contains("leaves `GraphOutput` node `video`"),
        "got {refusal}"
    );
}

#[test]
fn an_interface_node_naming_no_port_an_unknown_family_or_another_key_is_refused() {
    let declaring = |config: serde_json::Value| {
        refusal_of(serde_json::json!({
            "nodes": [{"name": "overlay", "type": GRAPH_INPUT_NODE_TYPE, "input_values": config}],
            "links": [],
        }))
    };
    assert!(declaring(serde_json::json!({"family": "image"})).contains("names no port"));
    let unknown_family = declaring(serde_json::json!({"port": "overlay", "family": "video"}));
    assert!(
        unknown_family.contains("\"video\"") && unknown_family.contains("records"),
        "got {unknown_family}"
    );
    let another_key =
        declaring(serde_json::json!({"port": "overlay", "family": "image", "name": "overlay"}));
    assert!(another_key.contains("`name`"), "got {another_key}");
}

#[test]
fn a_link_at_another_port_of_an_interface_node_is_refused() {
    let mut graph = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
    graph["links"][0]["target"]["port"] = serde_json::json!("video");
    let refusal = refusal_of(graph);
    assert!(refusal.contains("one port is `value`"), "got {refusal}");
}

#[test]
fn the_render_puts_each_interface_node_and_link_back_where_it_was_drawn() {
    let resolution =
        resolved(a_camera_offered_as_an_output_and_a_display_fed_by_an_input()).expect("resolves");
    let a_rendered_node = |name: &str| GraphNodeOutput {
        id: format!("{name}-id"),
        node_type: crate::core::descriptors::NodeClassImportPath::new("my_app:Node")
            .expect("import path"),
        kind: None,
        language: None,
        name: name.to_string(),
        config: Some(serde_json::json!({})),
        config_checksum: 0,
        ports: GraphNodePortsOnTheWire {
            inputs: Vec::new(),
            outputs: Vec::new(),
        },
        components: Default::default(),
        input_values: serde_json::Map::new(),
        live: None,
        pins: serde_json::Map::new(),
        exposed_config_fields: Vec::new(),
        studio: serde_json::Map::new(),
        inputs_waiting_for_a_value: Vec::new(),
        fragment: None,
    };
    let mut graph_response: GraphResponse = serde_json::from_value(serde_json::json!({
        "machine": "rig", "nodes": [], "links": [],
    }))
    .expect("an empty render parses");
    graph_response.nodes = vec![a_rendered_node("camera"), a_rendered_node("display")];
    resolution
        .interface_as_drawn
        .render_into(&mut graph_response);
    let names: Vec<&str> = graph_response
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(names, ["camera", "video", "overlay", "display"]);
    assert_eq!(
        serde_json::to_value(&graph_response.nodes[1].components.graph_interface).unwrap(),
        serde_json::json!({"direction": "output", "port": "video", "family": "image",
                           "pin_type": {"type": "texture"}})
    );
    assert_eq!(graph_response.links.len(), 2);
    assert_eq!(graph_response.links[0].target.node, "video");
    assert_eq!(graph_response.links[1].source.node, "overlay");
}

#[test]
fn a_node_whose_name_casts_like_an_interface_nodes_is_refused_as_a_name_taken() {
    let mut graph = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
    graph["nodes"][0]["name"] = serde_json::json!("Video");
    graph["links"][0]["source"]["node"] = serde_json::json!("Video");
    match resolved(graph) {
        Err(Error::NodeNameTaken { cast, .. }) => assert_eq!(cast, "video"),
        other => panic!("expected NodeNameTaken, got {other:?}"),
    }
}

/// A `GraphInput`'s `value` input takes the family and range each config names, and the ones
/// its type declares once a later config names none.
///
/// Mental-revert: resolving a new config against the ports the last config resolved keeps the
/// `array` family and the range it named.
#[test]
fn a_graph_inputs_value_takes_its_declared_family_back_once_a_config_names_none() {
    let descriptor =
        crate::core::nodes::the_node_type_of_a_graph_input().expect("the GraphInput type builds");
    let mut node = crate::core::graph::GraphNode::new(
        descriptor.node_class_import_path.clone(),
        None,
        "fader",
        Some(serde_json::json!({ "port": "fader", "family": "array", "range": [0, 1] })),
        descriptor
            .inputs
            .iter()
            .map(crate::core::graph::PortInfo::from)
            .collect(),
        descriptor
            .outputs
            .iter()
            .map(crate::core::graph::PortInfo::from)
            .collect(),
    );
    let value_input = |node: &crate::core::graph::GraphNode| {
        node.input_port(GRAPH_INTERFACE_VALUE_PORT_NAME)
            .expect("a GraphInput has a `value` input")
            .clone()
    };
    assert_eq!(value_input(&node).family.family, PortFamily::Array);
    assert!(value_input(&node).range().is_some());

    node.set_config(serde_json::json!({ "port": "fader" }));
    assert_eq!(value_input(&node).family.family, PortFamily::Records);
    assert_eq!(value_input(&node).range(), None);

    node.set_config(serde_json::json!({ "port": "fader", "family": "channels" }));
    assert_eq!(value_input(&node).family.family, PortFamily::Channels);
}

/// A `GraphInput`'s rendered config mirrors the value saved on its `value` input under `default`,
/// the key a published Tato Studio reads, and holds no `value` or `default` of its own, so a
/// cleared value renders no key.
///
/// Mental-revert: keeping a `default` the config itself holds renders a value a client cleared,
/// and a graph drawn back from it holds that value again.
#[test]
fn a_graph_input_renders_its_saved_value_under_default_and_a_cleared_one_under_no_key() {
    let graph_input = NodeClassImportPath::new(GRAPH_INPUT_NODE_TYPE.to_string()).unwrap();
    let saved = serde_json::json!({ "value": { "track": "drop" } });
    assert_eq!(
        the_config_a_graph_interface_node_renders(
            &graph_input,
            saved.as_object().unwrap(),
            serde_json::json!({ "port": "cue", "default": { "track": "intro" } }),
        ),
        serde_json::json!({ "port": "cue", "default": { "track": "drop" } })
    );
    let cleared = serde_json::json!({ "value": null });
    assert_eq!(
        the_config_a_graph_interface_node_renders(
            &graph_input,
            cleared.as_object().unwrap(),
            serde_json::json!({ "port": "cue", "value": { "track": "drop" },
                                "default": { "track": "intro" } }),
        ),
        serde_json::json!({ "port": "cue" })
    );
}

/// A top-level `GraphInput` whose `pin_type` chooses a Float runs as a source node, carries the
/// Channels family a Float travels on, and states the Float on its port and on the stream's
/// interface; one choosing a Texture runs nothing.
///
/// Mental-revert: reading the port's type from `family` alone leaves a Graph Input choosing a
/// Float waiting for a link, running nothing and stating no type.
#[test]
fn a_graph_input_choosing_a_pin_type_carries_it_on_its_port_and_on_the_interface() {
    let resolution = resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "pin_type": {"type": "float"}, "value": 0.5,
                           "range": [0, 1]}),
    ))
    .expect("a Graph Input choosing a Float resolves");
    let declared: Vec<&GraphInterfacePort> =
        resolution.interface_as_drawn.declared_ports().collect();
    assert!(declared[0].runs_as_a_source_node);
    assert_eq!(declared[0].family, Some(PortFamily::Channels));
    assert_eq!(
        declared[0].pin_type,
        Some(PinType::Float { exact_size: None })
    );
    assert_eq!(
        serde_json::to_value(declared[0].as_component()).unwrap(),
        serde_json::json!({"direction": "input", "port": "cue", "family": "channels",
                           "pin_type": {"type": "float"}})
    );
    let value_output = declared[0].rendered_as_a_port_named(GRAPH_INTERFACE_VALUE_PORT_NAME);
    assert_eq!(
        value_output.pin.map(|pin| pin.pin_type),
        Some(PinType::Float { exact_size: None })
    );

    let texture = resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "pin_type": {"type": "texture"}}),
    ))
    .expect("a Graph Input choosing a Texture resolves");
    assert!(
        texture
            .graph_the_runtime_runs
            .nodes
            .iter()
            .all(|node| node.name != "cue")
    );
}

/// A Graph Input choosing a type refuses a saved value its type does not hold, and a choice that
/// is no pin type.
///
/// Mental-revert: checking the saved value by the family alone takes a string on a Float Input,
/// since the Channels family it travels on is not consulted for a typed port.
#[test]
fn a_graph_input_choosing_a_pin_type_refuses_a_value_or_a_choice_it_does_not_take() {
    let refusal_on_the_top_level = |input_config: serde_json::Value| {
        resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(input_config))
            .expect_err("the input is refused")
            .to_string()
    };
    let string_on_a_float = refusal_on_the_top_level(
        serde_json::json!({"port": "cue", "pin_type": {"type": "float"}, "value": "loud"}),
    );
    assert!(
        string_on_a_float.contains("`cue`") && string_on_a_float.contains("Float"),
        "{string_on_a_float}"
    );
    let no_pin_type =
        refusal_on_the_top_level(serde_json::json!({"port": "cue", "pin_type": "float"}));
    assert!(no_pin_type.contains("`pin_type`"), "{no_pin_type}");
    let a_setting_type = refusal_on_the_top_level(serde_json::json!({
        "port": "cue", "pin_type": {"type": "pin_type_choice"}
    }));
    assert!(
        a_setting_type.contains("only a setting"),
        "{a_setting_type}"
    );
}

/// A Graph Output and a Graph Input render their settings as inputs, each reading the value it
/// holds, else its default, beside their `value` port.
///
/// Mental-revert: rendering an interface node with only its `value` port leaves Tato Studio no
/// `pin_type` row to draw the type choice control on.
#[test]
fn an_interface_node_renders_its_settings_as_inputs_reading_their_values() {
    let resolution = resolved(serde_json::json!({
        "nodes": [
            {"name": "camera", "type": "tatolab.camera:CameraSource"},
            {"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
             "input_values": {"port": "video", "pin_type": {"type": "texture"}}},
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"},
             "target": {"node": "video", "port": "value"}},
        ],
    }))
    .expect("the interface resolves");
    let output = resolution.interface_as_drawn.interface_nodes[0].rendered_as_a_node();
    let input_names: Vec<&str> = output
        .ports
        .inputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(input_names, ["port", "pin_type", "family", "value"]);
    let pin_type = &output.ports.inputs[1];
    assert_eq!(
        pin_type.pin.as_ref().map(|pin| pin.pin_type.clone()),
        Some(PinType::PinTypeChoice { among: Vec::new() })
    );
    let read = pin_type
        .value_read
        .as_ref()
        .expect("a setting reads its value");
    assert_eq!(read.value, Some(serde_json::json!({"type": "texture"})));
    assert_eq!(
        read.value_comes_from,
        Some(InputValueComesFromOnTheWire::Saved)
    );
    let family = output.ports.inputs[2]
        .value_read
        .as_ref()
        .expect("a setting reads its value");
    assert_eq!(family.value, Some(serde_json::Value::Null));
    assert_eq!(
        output.ports.inputs[3]
            .pin
            .as_ref()
            .map(|pin| pin.pin_type.clone()),
        Some(PinType::Texture {
            pixel_formats: Vec::new()
        })
    );
}

/// A Graph Output choosing a type refuses a source whose type the link rules do not connect to
/// it, naming both, and takes one they do.
///
/// Mental-revert: leaving the chosen type out of the link judgement answers no refusal for the
/// Texture source.
#[test]
fn a_graph_output_choosing_a_type_refuses_a_source_its_type_does_not_connect_to() {
    let resolved = resolved(serde_json::json!({
        "nodes": [
            {"name": "camera", "type": "tatolab.camera:CameraSource"},
            {"name": "level", "type": GRAPH_OUTPUT_NODE_TYPE,
             "input_values": {"port": "level", "pin_type": {"type": "float"}}},
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"},
             "target": {"node": "level", "port": "value"}},
        ],
    }))
    .expect("the interface resolves");
    let declared: Vec<&GraphInterfacePort> = resolved.interface_as_drawn.declared_ports().collect();
    let camera_video = crate::core::graph::PortInfo::from(
        &crate::core::descriptors::PortDescriptor::new(
            "video",
            "",
            false,
            PortFamilyDeclaration::of_family(PortFamily::Image),
        )
        .with_pin_declaration(crate::core::descriptors::PinDeclaration::exactly(
            PinType::Texture {
                pixel_formats: Vec::new(),
            },
        )),
    );
    let the_cameras_type =
        crate::core::descriptors::NodeClassImportPath::new("nodes.camera:Camera")
            .expect("an import path");
    let (chosen, refusal) = declared[0]
        .refusal_of_a_linked_port_by_the_chosen_type(
            &camera_video,
            &the_cameras_type,
            &declared[0].linked_ports[0],
        )
        .expect("a Texture does not connect to a Float");
    assert_eq!(chosen, &PinType::Float { exact_size: None });
    assert!(refusal.contains("camera"), "{refusal}");

    let float_source = crate::core::graph::PortInfo::from(
        &crate::core::descriptors::PortDescriptor::new(
            "video",
            "",
            false,
            PortFamilyDeclaration::of_family(PortFamily::Channels),
        )
        .with_pin_declaration(crate::core::descriptors::PinDeclaration::exactly(
            PinType::Boolean {},
        )),
    );
    assert_eq!(
        declared[0].refusal_of_a_linked_port_by_the_chosen_type(
            &float_source,
            &the_cameras_type,
            &declared[0].linked_ports[0]
        ),
        None
    );
}

/// A Graph Input saved before `pin_type` existed, naming a family alone, declares the port it
/// declared: of its family, and of a Texture where that family is Image.
///
/// Mental-revert: deriving the interface's `pin_type` from the chosen type alone states none for
/// a saved Image Input, which a family declaration states as a Texture.
#[test]
fn a_graph_input_saved_with_a_family_alone_declares_the_port_it_declared() {
    let resolution = resolved(a_camera_offered_as_an_output_and_a_display_fed_by_an_input())
        .expect("the interface resolves");
    let declared: Vec<&GraphInterfacePort> =
        resolution.interface_as_drawn.declared_ports().collect();
    assert_eq!(declared[1].pin_type, None);
    assert_eq!(
        declared[1].the_pin_type_it_carries(),
        Some(PinType::Texture {
            pixel_formats: Vec::new()
        })
    );
    let channels = resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "family": "channels", "lifetime": "reset"}),
    ))
    .expect("a saved Channels Input resolves");
    let declared: Vec<&GraphInterfacePort> = channels.interface_as_drawn.declared_ports().collect();
    assert!(declared[0].runs_as_a_source_node);
    assert_eq!(declared[0].the_pin_type_it_carries(), None);
}

/// A Graph Input choosing a Texture carries it in a Texture's default flow, a stream, on its
/// node's `value` ports and on the port the interface renders.
///
/// Mental-revert: writing the chosen type onto the `value` ports in the value flow a stored
/// value takes renders a Texture Input's port as a value, which no Texture output feeds.
#[test]
fn a_graph_input_choosing_a_texture_carries_it_as_a_stream() {
    let descriptor =
        crate::core::nodes::the_node_type_of_a_graph_input().expect("the GraphInput type builds");
    let node = crate::core::graph::GraphNode::new(
        descriptor.node_class_import_path.clone(),
        None,
        "picture",
        Some(serde_json::json!({ "port": "picture", "pin_type": {"type": "texture"} })),
        descriptor
            .inputs
            .iter()
            .map(crate::core::graph::PortInfo::from)
            .collect(),
        descriptor
            .outputs
            .iter()
            .map(crate::core::graph::PortInfo::from)
            .collect(),
    );
    let value_output = node
        .output_port(GRAPH_INTERFACE_VALUE_PORT_NAME)
        .expect("a GraphInput has a `value` output");
    assert_eq!(value_output.family.family, PortFamily::Image);
    assert_eq!(
        value_output.pin.as_ref().map(|pin| pin.flow),
        Some(crate::core::descriptors::PinFlow::Stream)
    );

    let resolution = resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "pin_type": {"type": "texture"}}),
    ))
    .expect("a Graph Input choosing a Texture resolves");
    let declared: Vec<&GraphInterfacePort> =
        resolution.interface_as_drawn.declared_ports().collect();
    assert_eq!(
        declared[0]
            .rendered_as_a_port_named(GRAPH_INTERFACE_VALUE_PORT_NAME)
            .pin
            .map(|pin| pin.flow),
        Some(crate::core::descriptors::PinFlow::Stream)
    );
}

/// A top-level Graph Input choosing a List, which no family's message carries, is refused naming
/// the node.
///
/// Mental-revert: leaving the family of a chosen type no family carries unset resolves the Input
/// as one waiting for a link, and the List is taken.
#[test]
fn a_top_level_graph_input_choosing_a_type_no_family_carries_is_refused_naming_it() {
    let refusal = resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "pin_type": {
            "type": "list_of", "element_type": {"type": "float"}
        }}),
    ))
    .expect_err("a List Input is refused")
    .to_string();
    assert!(refusal.contains("`GraphInput` node `"), "{refusal}");
    assert!(
        refusal.contains("which no stream's port carries yet"),
        "{refusal}"
    );
}

/// A Graph Input saved with a Channels family and a `reset` lifetime renders its `lifetime`
/// setting reading `reset`, saved.
///
/// Mental-revert: rendering only the settings every interface node takes drops `lifetime` from a
/// Graph Input's inputs.
#[test]
fn a_graph_input_renders_the_lifetime_it_saves() {
    let resolution = resolved_on_the_streams_top_level(a_records_input_read_by_two_nodes(
        serde_json::json!({"port": "cue", "family": "channels", "lifetime": "reset"}),
    ))
    .expect("a saved Channels Input resolves");
    let rendered = resolution.interface_as_drawn.interface_nodes[0].rendered_as_a_node();
    let lifetime = rendered
        .ports
        .inputs
        .iter()
        .find(|input| input.name == GRAPH_INPUT_LIFETIME_CONFIG_KEY)
        .and_then(|input| input.value_read.as_ref())
        .expect("`lifetime` renders reading its value");
    assert_eq!(lifetime.value, Some(serde_json::json!("reset")));
    assert_eq!(
        lifetime.value_comes_from,
        Some(InputValueComesFromOnTheWire::Saved)
    );
}
