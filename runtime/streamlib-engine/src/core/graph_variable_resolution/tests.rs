// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn resolved(graph: serde_json::Value) -> Result<GraphSnapshotWithVariablesResolved> {
    GraphSnapshot::from_graph_document(graph)
        .expect("the graph parses")
        .with_graph_variables_resolved(&GraphFragmentsAsDrawn::default())
}

fn refusal_of(graph: serde_json::Value) -> String {
    resolved(graph)
        .expect_err("the graph's variables are refused")
        .to_string()
}

fn link(source: (&str, &str), target: (&str, &str)) -> GraphSnapshotLink {
    GraphSnapshotLink {
        source: LinkPortRefOutput {
            node: source.0.to_string(),
            port: source.1.to_string(),
        },
        target: LinkPortRefOutput {
            node: target.0.to_string(),
            port: target.1.to_string(),
        },
    }
}

fn a_camera_variable_read_by_two_displays() -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            {"name": "camera", "type": "rig:Camera"},
            {"name": "set-camera", "type": "tatolab.stream:SetVariable", "config": {"variable": "camera"}},
            {"name": "get-camera", "type": "tatolab.stream:GetVariable", "config": {"variable": "camera"}},
            {"name": "get-camera-2", "type": "tatolab.stream:GetVariable", "config": {"variable": "camera"}},
            {"name": "front", "type": "rig:Display"},
            {"name": "back", "type": "rig:Display"}
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"}, "target": {"node": "set-camera", "port": "value"}},
            {"source": {"node": "get-camera", "port": "value"}, "target": {"node": "front", "port": "video_in"}},
            {"source": {"node": "get-camera-2", "port": "value"}, "target": {"node": "back", "port": "video_in"}}
        ]
    })
}

#[test]
fn a_port_variable_read_twice_resolves_to_two_links_from_the_setters_source() {
    let resolution = resolved(a_camera_variable_read_by_two_displays()).unwrap();
    let runtime = &resolution.graph_the_runtime_runs;
    let node_names: Vec<&str> = runtime.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(node_names, ["camera", "front", "back"]);
    assert_eq!(
        runtime.links,
        [
            link(("camera", "video"), ("front", "video_in")),
            link(("camera", "video"), ("back", "video_in")),
        ]
    );
    assert_eq!(resolution.variables_as_drawn.variable_nodes.len(), 3);
    assert_eq!(
        resolution
            .variables_as_drawn
            .links_touching_a_variable_node
            .len(),
        3
    );
}

#[test]
fn a_graph_with_no_variable_resolves_to_itself() {
    let graph = serde_json::json!({
        "nodes": [{"name": "camera", "type": "rig:Camera"}, {"name": "front", "type": "rig:Display"}],
        "links": [{"source": {"node": "camera", "port": "video"}, "target": {"node": "front", "port": "video_in"}}]
    });
    let resolution = resolved(graph.clone()).unwrap();
    assert_eq!(
        resolution.graph_the_runtime_runs,
        GraphSnapshot::from_graph_document(graph).unwrap()
    );
    assert_eq!(
        resolution.variables_as_drawn,
        GraphVariablesAsDrawn::default()
    );
}

#[test]
fn a_get_with_no_setter_is_refused_naming_it_and_the_variables_set() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["nodes"][3]["config"]["variable"] = serde_json::json!("microphone");
    let refusal = refusal_of(graph);
    assert!(
        refusal.contains(
            "`GetVariable` node `get-camera-2` reads the variable `microphone`, which no \
                 `SetVariable` on its level or an enclosing one sets. It can read: camera"
        ),
        "{refusal}"
    );
}

fn resolved_once_flattened(graph: serde_json::Value) -> Result<GraphSnapshotWithVariablesResolved> {
    let flattening = GraphSnapshot::from_graph_document(graph)
        .expect("the graph parses")
        .with_graph_fragments_flattened(&crate::core::processors::NodeTypesOneStreamResolves::new())
        .expect("the fragments flatten");
    flattening
        .graph_the_runtime_runs
        .with_graph_variables_resolved(&flattening.fragments_as_drawn)
}

fn a_fragment_whose_tint_reads_the_variable_shade(
    nodes_beside_the_tint: serde_json::Value,
    links_beside_the_tints: serde_json::Value,
) -> serde_json::Value {
    let mut nodes = vec![
        serde_json::json!({"name": "source", "type": "tatolab.stream:GraphInput",
                               "config": {"port": "video", "family": "image"}}),
        serde_json::json!({"name": "get-shade", "type": "tatolab.stream:GetVariable",
                               "config": {"variable": "shade"}}),
        serde_json::json!({"name": "tint", "type": "rig:Tint"}),
        serde_json::json!({"name": "out", "type": "tatolab.stream:GraphOutput",
                               "config": {"port": "video", "family": "image"}}),
    ];
    nodes.extend(nodes_beside_the_tint.as_array().unwrap().iter().cloned());
    let mut links = vec![
        serde_json::json!({"source": {"node": "source", "port": "value"},
                               "target": {"node": "tint", "port": "video_in"}}),
        serde_json::json!({"source": {"node": "get-shade", "port": "value"},
                               "target": {"node": "tint", "port": "shade_in"}}),
        serde_json::json!({"source": {"node": "tint", "port": "video"},
                               "target": {"node": "out", "port": "value"}}),
    ];
    links.extend(links_beside_the_tints.as_array().unwrap().iter().cloned());
    serde_json::json!({"nodes": nodes, "links": links})
}

fn a_shade_set_inside() -> serde_json::Value {
    a_fragment_whose_tint_reads_the_variable_shade(
        serde_json::json!([
            {"name": "palette", "type": "rig:Palette"},
            {"name": "set-shade", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "shade"}},
        ]),
        serde_json::json!([
            {"source": {"node": "palette", "port": "colour"},
             "target": {"node": "set-shade", "port": "value"}},
        ]),
    )
}

fn a_camera_through(
    fragments: &[(&str, serde_json::Value)],
    top_level_nodes: serde_json::Value,
    top_level_links: serde_json::Value,
) -> serde_json::Value {
    let mut nodes = vec![serde_json::json!({"name": "camera", "type": "rig:Camera"})];
    let mut links = Vec::new();
    let mut previous = ("camera", "video");
    for (fragment_name, fragment) in fragments {
        nodes.push(
            serde_json::json!({"name": fragment_name, "type": "looks.generators:look",
                                          "fragment": fragment}),
        );
        links.push(
            serde_json::json!({"source": {"node": previous.0, "port": previous.1},
                                          "target": {"node": fragment_name, "port": "video"}}),
        );
        previous = (fragment_name, "video");
    }
    nodes.push(serde_json::json!({"name": "display", "type": "rig:Display"}));
    links.push(
        serde_json::json!({"source": {"node": previous.0, "port": previous.1},
                                      "target": {"node": "display", "port": "video_in"}}),
    );
    nodes.extend(top_level_nodes.as_array().unwrap().iter().cloned());
    links.extend(top_level_links.as_array().unwrap().iter().cloned());
    serde_json::json!({"nodes": nodes, "links": links})
}

#[test]
fn a_fragment_placed_twice_reads_each_placements_own_variable() {
    let resolution = resolved_once_flattened(a_camera_through(
        &[
            ("warm", a_shade_set_inside()),
            ("cool", a_shade_set_inside()),
        ],
        serde_json::json!([]),
        serde_json::json!([]),
    ))
    .expect("each placement sets its own `shade`");
    let links = &resolution.graph_the_runtime_runs.links;
    assert!(links.contains(&link(("warm/palette", "colour"), ("warm/tint", "shade_in"))));
    assert!(links.contains(&link(("cool/palette", "colour"), ("cool/tint", "shade_in"))));
}

#[test]
fn a_get_inside_a_fragment_with_no_setter_on_its_level_reads_the_enclosing_one() {
    let resolution = resolved_once_flattened(a_camera_through(
        &[(
            "look",
            a_fragment_whose_tint_reads_the_variable_shade(
                serde_json::json!([]),
                serde_json::json!([]),
            ),
        )],
        serde_json::json!([
            {"name": "palette", "type": "rig:Palette"},
            {"name": "set-shade", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "shade"}},
        ]),
        serde_json::json!([
            {"source": {"node": "palette", "port": "colour"},
             "target": {"node": "set-shade", "port": "value"}},
        ]),
    ))
    .expect("the get reads the stream's `shade`");
    assert!(
        resolution
            .graph_the_runtime_runs
            .links
            .contains(&link(("palette", "colour"), ("look/tint", "shade_in")))
    );
}

#[test]
fn a_setter_inside_a_fragment_shadows_the_enclosing_one_for_its_own_level_only() {
    let resolution = resolved_once_flattened(a_camera_through(
        &[("look", a_shade_set_inside())],
        serde_json::json!([
            {"name": "grey", "type": "rig:Palette"},
            {"name": "set-shade", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "shade"}},
            {"name": "get-shade", "type": "tatolab.stream:GetVariable",
             "config": {"variable": "shade"}},
            {"name": "swatch", "type": "rig:Display"},
        ]),
        serde_json::json!([
            {"source": {"node": "grey", "port": "colour"},
             "target": {"node": "set-shade", "port": "value"}},
            {"source": {"node": "get-shade", "port": "value"},
             "target": {"node": "swatch", "port": "video_in"}},
        ]),
    ))
    .expect("one `shade` per level");
    let links = &resolution.graph_the_runtime_runs.links;
    assert!(links.contains(&link(("look/palette", "colour"), ("look/tint", "shade_in"))));
    assert!(links.contains(&link(("grey", "colour"), ("swatch", "video_in"))));
}

#[test]
fn a_get_outside_a_fragment_never_reads_a_variable_set_inside_it() {
    let refusal = resolved_once_flattened(a_camera_through(
        &[("look", a_shade_set_inside())],
        serde_json::json!([
            {"name": "get-shade", "type": "tatolab.stream:GetVariable",
             "config": {"variable": "shade"}},
            {"name": "swatch", "type": "rig:Display"},
        ]),
        serde_json::json!([
            {"source": {"node": "get-shade", "port": "value"},
             "target": {"node": "swatch", "port": "video_in"}},
        ]),
    ))
    .expect_err("the stream sets no `shade`")
    .to_string();
    assert!(
        refusal.contains(
            "`GetVariable` node `get-shade` reads the variable `shade`, which no \
                 `SetVariable` on its level or an enclosing one sets. It can read: no variable"
        ),
        "{refusal}"
    );
}

#[test]
fn a_variable_set_inside_a_fragment_leaves_it_through_a_graph_output() {
    let fragment_handing_out_its_shade = a_fragment_whose_tint_reads_the_variable_shade(
        serde_json::json!([
            {"name": "palette", "type": "rig:Palette"},
            {"name": "set-shade", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "shade"}},
            {"name": "get-shade-out", "type": "tatolab.stream:GetVariable",
             "config": {"variable": "shade"}},
            {"name": "shade-out", "type": "tatolab.stream:GraphOutput",
             "config": {"port": "shade", "family": "image"}},
        ]),
        serde_json::json!([
            {"source": {"node": "palette", "port": "colour"},
             "target": {"node": "set-shade", "port": "value"}},
            {"source": {"node": "get-shade-out", "port": "value"},
             "target": {"node": "shade-out", "port": "value"}},
        ]),
    );
    let resolution = resolved_once_flattened(a_camera_through(
        &[("look", fragment_handing_out_its_shade)],
        serde_json::json!([{"name": "swatch", "type": "rig:Display"}]),
        serde_json::json!([
            {"source": {"node": "look", "port": "shade"},
             "target": {"node": "swatch", "port": "video_in"}},
        ]),
    ))
    .expect("the stream reads the fragment's `shade` through its Output");
    assert!(
        resolution
            .graph_the_runtime_runs
            .links
            .contains(&link(("look/palette", "colour"), ("swatch", "video_in")))
    );
}

#[test]
fn two_setters_of_one_variable_are_refused_naming_both() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(
            {"name": "set-camera-2", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "camera", "node_reference": {"node": "front"}}}
        ));
    let refusal = refusal_of(graph);
    assert!(
        refusal.contains("the variable `camera` is set by both `set-camera` and `set-camera-2`"),
        "{refusal}"
    );
}

#[test]
fn a_setter_holding_nothing_or_two_values_is_refused() {
    let mut holding_nothing = a_camera_variable_read_by_two_displays();
    holding_nothing["links"].as_array_mut().unwrap().remove(0);
    assert!(refusal_of(holding_nothing).contains("sets the variable `camera` to nothing"));

    let mut holding_two = a_camera_variable_read_by_two_displays();
    holding_two["nodes"][1]["config"]["node_reference"] = serde_json::json!({"node": "front"});
    assert!(refusal_of(holding_two).contains("both from a link and from its `node_reference`"));
}

fn a_gateway_variable_read_by_a_tracks_config() -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            {"name": "cloudflare", "type": "rig:Gateway"},
            {"name": "set-connection", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "connection", "node_reference": {"node": "cloudflare"}}},
            {"name": "get-connection", "type": "tatolab.stream:GetVariable", "config": {"variable": "connection"}},
            {"name": "camera-track", "type": "rig:Track",
             "config": {"gateway": {"node": "get-connection"}, "track": "camera"}}
        ]
    })
}

#[test]
fn a_node_reference_variable_resolves_each_config_naming_a_get_to_the_setters_node() {
    let resolution = resolved(a_gateway_variable_read_by_a_tracks_config()).unwrap();
    let track = &resolution.graph_the_runtime_runs.nodes[1];
    assert_eq!(track.name, "camera-track");
    assert_eq!(
        track.config,
        serde_json::json!({"gateway": {"node": "cloudflare"}, "track": "camera"})
    );
    assert_eq!(
        resolution
            .variables_as_drawn
            .configs_naming_a_get_by_node_name["camera-track"]
            .as_drawn,
        serde_json::json!({"gateway": {"node": "get-connection"}, "track": "camera"})
    );
}

#[test]
fn a_get_read_as_the_kind_of_value_its_variable_does_not_hold_is_refused() {
    let mut linked_reference = a_gateway_variable_read_by_a_tracks_config();
    linked_reference["links"] = serde_json::json!([
        {"source": {"node": "get-connection", "port": "value"}, "target": {"node": "camera-track", "port": "content"}}
    ]);
    assert!(
        refusal_of(linked_reference).contains("its variable `connection` holds a node reference")
    );

    let mut config_reading_a_port = a_camera_variable_read_by_two_displays();
    config_reading_a_port["nodes"][4]["config"] =
        serde_json::json!({"source": {"node": "get-camera"}});
    assert!(
        refusal_of(config_reading_a_port).contains("its variable `camera` holds a port's output")
    );
}

#[test]
fn a_link_out_of_a_setter_or_onto_another_port_is_refused() {
    let mut out_of_a_setter = a_camera_variable_read_by_two_displays();
    out_of_a_setter["links"][1]["source"]["node"] = serde_json::json!("set-camera");
    assert!(refusal_of(out_of_a_setter).contains("a link leaves `SetVariable` node `set-camera`"));

    let mut another_port = a_camera_variable_read_by_two_displays();
    another_port["links"][1]["source"]["port"] = serde_json::json!("video");
    assert!(refusal_of(another_port).contains("one port is `value`"));
}

#[test]
fn a_variable_node_naming_no_variable_or_an_unknown_key_is_refused() {
    let mut no_variable = a_camera_variable_read_by_two_displays();
    no_variable["nodes"][2]["config"] = serde_json::json!({});
    assert!(refusal_of(no_variable).contains("`get-camera` names no variable"));

    let mut unknown_key = a_camera_variable_read_by_two_displays();
    unknown_key["nodes"][2]["config"]["colour"] = serde_json::json!("red");
    assert!(refusal_of(unknown_key).contains("carries the config key `colour`"));
}
#[test]
fn a_variable_node_whose_name_casts_like_another_nodes_is_refused() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["nodes"][2]["name"] = serde_json::json!("Camera");
    let refusal = resolved(graph).unwrap_err();
    assert!(
        matches!(refusal, Error::NodeNameTaken { ref cast, .. } if cast == "camera"),
        "{refusal}"
    );
}

#[test]
fn a_link_into_a_get_is_refused_naming_where_a_variable_is_set() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["links"].as_array_mut().unwrap().push(serde_json::json!(
            {"source": {"node": "camera", "port": "video"}, "target": {"node": "get-camera", "port": "value"}}
        ));
    assert!(refusal_of(graph).contains("a link enters `GetVariable` node `get-camera`"));
}

#[test]
fn a_get_no_link_or_config_reads_yet_still_needs_a_setter() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["nodes"].as_array_mut().unwrap().push(serde_json::json!(
            {"name": "get-camra", "type": "tatolab.stream:GetVariable", "config": {"variable": "camra"}}
        ));
    assert!(
        refusal_of(graph)
            .contains("reads the variable `camra`, which no `SetVariable` on its level")
    );
}

#[test]
fn a_node_reference_naming_a_get_at_any_depth_resolves() {
    let mut graph = a_gateway_variable_read_by_a_tracks_config();
    graph["nodes"][3]["config"] =
        serde_json::json!({"routes": [{"gateway": {"node": "get-connection"}}]});
    let resolution = resolved(graph).unwrap();
    assert_eq!(
        resolution.graph_the_runtime_runs.nodes[1].config,
        serde_json::json!({"routes": [{"gateway": {"node": "cloudflare"}}]})
    );
}

#[test]
fn a_setter_holding_another_variable_node_is_refused() {
    let mut graph = a_gateway_variable_read_by_a_tracks_config();
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(
            {"name": "set-alias", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "alias", "node_reference": {"node": "get-connection"}}}
        ));
    assert!(refusal_of(graph).contains("holds variable node `get-connection`"));
}
fn a_rendered_node(name: &str) -> ProcessorNodeOutput {
    ProcessorNodeOutput {
        id: format!("id-{name}"),
        processor_type: ProcessorClassImportPath::new("rig:Node").unwrap(),
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
    }
}

fn a_rendered_link(id: &str, source: (&str, &str), target: (&str, &str)) -> LinkOutput {
    let GraphSnapshotLink { source, target } = link(source, target);
    LinkOutput {
        id: id.to_string(),
        source,
        target,
        capacity: 4,
        state: Default::default(),
        error_reason: None,
        components: serde_json::Map::new(),
    }
}

#[test]
fn a_variable_port_spelled_another_way_that_casts_to_value_resolves() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["links"][0]["target"]["port"] = serde_json::json!("VALUE");
    graph["links"][1]["source"]["port"] = serde_json::json!("Value");
    let resolution = resolved(graph).unwrap();
    assert_eq!(resolution.graph_the_runtime_runs.links.len(), 2);
}

#[test]
fn a_config_object_naming_no_castable_node_is_left_as_written() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["nodes"][4]["config"] = serde_json::json!({"metadata": {"node": ""}});
    let resolution = resolved(graph).unwrap();
    assert_eq!(
        resolution.graph_the_runtime_runs.nodes[1].config,
        serde_json::json!({"metadata": {"node": ""}})
    );
}

#[test]
fn the_render_matches_a_runtime_link_whose_ports_the_runtime_cast() {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["links"][0]["source"]["port"] = serde_json::json!("VIDEO");
    let resolution = resolved(graph).unwrap();
    let mut graph_response = GraphResponse {
        asset: None,
        display_name: None,
        stream: None,
        clock: Default::default(),
        nodes: vec![a_rendered_node("camera"), a_rendered_node("back")],
        links: vec![a_rendered_link(
            "L2",
            ("camera", "video"),
            ("back", "video_in"),
        )],
        references: Vec::new(),
        machine: "vm".to_string(),
        groups: Vec::new(),
        studio: None,
    };
    resolution
        .variables_as_drawn
        .render_into(&mut graph_response);
    assert!(
        graph_response.links.iter().all(
            |rendered| rendered.source.node != "camera" || rendered.target.node == "set-camera"
        ),
        "{:?}",
        graph_response.links
    );
    assert_ne!(graph_response.nodes[1].config_checksum, 0);
}

#[test]
fn the_render_carries_each_runtime_links_id_and_drops_a_drawn_link_whose_reader_is_gone() {
    let resolution = resolved(a_camera_variable_read_by_two_displays()).unwrap();
    let mut graph_response = GraphResponse {
        asset: None,
        display_name: None,
        stream: None,
        clock: Default::default(),
        nodes: vec![a_rendered_node("camera"), a_rendered_node("back")],
        links: vec![a_rendered_link(
            "L2",
            ("camera", "video"),
            ("back", "video_in"),
        )],
        references: Vec::new(),
        machine: "vm".to_string(),
        groups: Vec::new(),
        studio: None,
    };
    resolution
        .variables_as_drawn
        .render_into(&mut graph_response);

    let rendered_links: Vec<(String, String, String)> = graph_response
        .links
        .iter()
        .map(|rendered| {
            (
                rendered.id.clone(),
                rendered.source.node.clone(),
                rendered.target.node.clone(),
            )
        })
        .collect();
    assert_eq!(
        rendered_links,
        [
            (
                String::new(),
                "camera".to_string(),
                "set-camera".to_string()
            ),
            (
                "L2".to_string(),
                "get-camera-2".to_string(),
                "back".to_string()
            ),
        ]
    );
    let rendered_nodes: Vec<&str> = graph_response
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(
        rendered_nodes,
        ["camera", "set-camera", "get-camera", "get-camera-2", "back"]
    );
    assert_eq!(graph_response.nodes[1].ports.inputs[0].name, "value");
    assert_eq!(graph_response.nodes[2].ports.outputs[0].name, "value");
}
