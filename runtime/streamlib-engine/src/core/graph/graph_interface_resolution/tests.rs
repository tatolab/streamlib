// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
            {"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}},
            {"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
             "config": {"port": "video", "family": "image"}},
            {"name": "overlay", "type": GRAPH_INPUT_NODE_TYPE,
             "config": {"port": "overlay", "family": "image"}},
            {"name": "display", "type": "tatolab.display:Display", "config": {}},
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
            {"name": "cue", "type": GRAPH_INPUT_NODE_TYPE, "config": input_config},
            {"name": "left", "type": "tatolab.test:Reader", "config": {}},
            {"name": "right", "type": "tatolab.test:Reader", "config": {}},
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
            {"name": "input1", "type": GRAPH_INPUT_NODE_TYPE, "config": {"port": "input1"}},
            {"name": "output1", "type": GRAPH_OUTPUT_NODE_TYPE, "config": {"port": "output1"}},
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
        rendered.components[GRAPH_INTERFACE_COMPONENT_KEY]["family"],
        serde_json::Value::Null
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
            serde_json::json!({"name": "camera", "type": "tatolab.camera:CameraSource",
                                   "config": {}}),
            serde_json::json!({"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
                                   "config": {"port": "video"}}),
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
        cue_as_it_runs.config,
        serde_json::json!({"port": "cue", "family": "channels"})
    );
    assert_eq!(
        resolution.interface_as_drawn.interface_nodes[0].node.config,
        serde_json::json!({"port": "cue"}),
        "the node renders back as drawn"
    );
}

#[test]
fn an_output_following_an_input_fed_by_a_graph_input_naming_no_family_carries_that_inputs_family() {
    use crate::core::descriptors::{
        PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
    };
    use crate::core::processors::PROCESSOR_REGISTRY;

    const PASSER_TYPE: &str = "interface_resolution_fixtures.follows_its_input:Passer";
    const READER_TYPE: &str = "interface_resolution_fixtures.reads_channels:Reader";
    let register = |node_type: &str, inputs: Vec<PortDescriptor>, outputs: Vec<PortDescriptor>| {
        let class_name = node_type
            .rsplit(':')
            .next()
            .expect("the type names a class");
        let mut descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new(class_name).expect("the class name is short"),
            ProcessorClassImportPath::new(node_type).expect("the type is an import path"),
            class_name,
        );
        descriptor.inputs = inputs;
        descriptor.outputs = outputs;
        PROCESSOR_REGISTRY
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
            {"name": "cue", "type": GRAPH_INPUT_NODE_TYPE, "config": {"port": "cue"}},
            {"name": "reader", "type": READER_TYPE, "config": {}},
            {"name": "passer", "type": PASSER_TYPE, "config": {}},
            {"name": "out", "type": GRAPH_OUTPUT_NODE_TYPE, "config": {"port": "out"}},
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
        "nodes": [{"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}}],
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
            {"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}},
            {"name": "first", "type": GRAPH_OUTPUT_NODE_TYPE,
             "config": {"port": "Video", "family": "image"}},
            {"name": "second", "type": GRAPH_OUTPUT_NODE_TYPE,
             "config": {"port": "video", "family": "image"}},
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
            {"name": "effect", "type": "my_app:Effect", "config": {}},
            {"name": "video-in", "type": GRAPH_INPUT_NODE_TYPE,
             "config": {"port": "video", "family": "image"}},
            {"name": "video-out", "type": GRAPH_OUTPUT_NODE_TYPE,
             "config": {"port": "video", "family": "image"}},
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
                   "config": {"port": "video", "family": "image"}}],
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
        refusal.contains("enters `GraphInput` node `overlay`"),
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
            "nodes": [{"name": "overlay", "type": GRAPH_INPUT_NODE_TYPE, "config": config}],
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
    let a_rendered_node = |name: &str| ProcessorNodeOutput {
        id: format!("{name}-id"),
        processor_type: crate::core::descriptors::ProcessorClassImportPath::new("my_app:Node")
            .expect("import path"),
        kind: None,
        language: None,
        name: name.to_string(),
        config: Some(serde_json::json!({})),
        config_checksum: 0,
        ports: ProcessorNodePortsOutput {
            inputs: Vec::new(),
            outputs: Vec::new(),
        },
        components: serde_json::Map::new(),
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
        graph_response.nodes[1].components[GRAPH_INTERFACE_COMPONENT_KEY],
        serde_json::json!({"direction": "output", "port": "video", "family": "image"})
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
