// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph_interface_resolution::{GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE};

const LOOK_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a01";
const SOFT_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a02";
const BLUR_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a03";
const TINT_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a04";

const TINT_NODE_TYPE: &str = "looks.nodes:Tint";

/// A stream that described `Tint`, whose config schema marks `reads_radius_of` alone as a
/// node reference.
fn node_types_describing_a_tint_that_reads_a_radius_by_reference() -> NodeTypesOneStreamResolves {
    let node_types = NodeTypesOneStreamResolves::new();
    let descriptor = crate::core::descriptors::ProcessorDescriptor::new(
        crate::core::descriptors::ProcessorClassShortName::new("Tint").unwrap(),
        crate::core::descriptors::ProcessorClassImportPath::new(TINT_NODE_TYPE).unwrap(),
        "fragment flattening test",
    )
    .with_config_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "reads_radius_of": {streamlib_processor_schema::NODE_REFERENCE_SCHEMA_KEYWORD: {}},
            "label_shaped_like_a_reference": {"type": "object"},
        }
    }));
    node_types
        .register_a_type_described_in_this_streams_processor_interpreter(
            descriptor,
            crate::core::processors::HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
                Box::new(|_node| Err(Error::NotSupported("never constructed".into()))),
            ),
        )
        .expect("the type is described into the stream");
    node_types
}

fn blur_then_tint_fragment() -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            {"name": "source", "type": GRAPH_INPUT_NODE_TYPE,
             "config": {"port": "video", "family": "image"}},
            {"id": BLUR_UUID, "name": "blur", "type": "looks.nodes:Blur",
             "config": {"radius": 8}},
            {"id": TINT_UUID, "name": "tint", "type": TINT_NODE_TYPE,
             "config": {
                 "reads_radius_of": {"node": "blur"},
                 "label_shaped_like_a_reference": {"node": "blur"},
             }},
            {"name": "out", "type": GRAPH_OUTPUT_NODE_TYPE,
             "config": {"port": "video", "family": "image"}},
        ],
        "links": [
            {"source": {"node": "source", "port": "value"},
             "target": {"node": BLUR_UUID, "port": "video"}},
            {"source": {"node": "blur", "port": "video_out"},
             "target": {"node": "tint", "port": "video"}},
            {"source": {"node": "tint", "port": "video_out"},
             "target": {"node": "out", "port": "value"}},
        ],
    })
}

fn a_camera_through_look_holding_soft_into_a_display() -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            {"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}},
            {"id": LOOK_UUID, "name": "look", "type": "looks.generators:look",
             "config": {}, "studio": {"x": 40},
             "fragment": {
                 "nodes": [
                     {"name": "source", "type": GRAPH_INPUT_NODE_TYPE,
                      "config": {"port": "video", "family": "image"}},
                     {"id": SOFT_UUID, "name": "soft",
                      "type": "looks.generators:blur_then_tint",
                      "config": {"radius": 8},
                      "fragment": blur_then_tint_fragment()},
                     {"name": "out", "type": GRAPH_OUTPUT_NODE_TYPE,
                      "config": {"port": "video", "family": "image"}},
                 ],
                 "links": [
                     {"source": {"node": "source", "port": "value"},
                      "target": {"node": SOFT_UUID, "port": "video"}},
                     {"source": {"node": "soft", "port": "video"},
                      "target": {"node": "out", "port": "value"}},
                 ],
             }},
            {"name": "display", "type": "tatolab.display:Display", "config": {}},
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"},
             "target": {"node": LOOK_UUID, "port": "video"}},
            {"source": {"node": "look", "port": "video"},
             "target": {"node": "display", "port": "video"}},
        ],
    })
}

fn flattened(graph: serde_json::Value) -> Result<GraphSnapshotWithFragmentsFlattened> {
    GraphSnapshot::from_graph_document(graph).and_then(|graph| {
        graph.with_graph_fragments_flattened(
            &node_types_describing_a_tint_that_reads_a_radius_by_reference(),
        )
    })
}

fn link_named(source: (&str, &str), target: (&str, &str)) -> GraphSnapshotLink {
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

#[test]
fn a_nested_fragment_flattens_under_its_path_of_names_keeping_each_nodes_uuid() {
    let flattening =
        flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
    let graph = &flattening.graph_the_runtime_runs;
    let names: Vec<&str> = graph.nodes.iter().map(|node| node.name.as_str()).collect();
    assert_eq!(
        names,
        ["camera", "display", "look/soft/blur", "look/soft/tint"]
    );
    assert_eq!(
        graph.nodes[2].id.map(|id| id.to_string()).as_deref(),
        Some(BLUR_UUID)
    );
    assert_eq!(
        graph.nodes[3].id.map(|id| id.to_string()).as_deref(),
        Some(TINT_UUID)
    );
    assert_eq!(
        graph.links,
        [
            link_named(("camera", "video"), ("look/soft/blur", "video")),
            link_named(("look/soft/tint", "video_out"), ("display", "video")),
            link_named(("look/soft/blur", "video_out"), ("look/soft/tint", "video")),
        ]
    );
    assert_eq!(
        flattening
            .fragments_as_drawn
            .fragment_names_flattened_and_cast(),
        HashSet::from(["look".to_string(), "look-soft".to_string()])
    );
}

#[test]
fn a_node_reference_inside_a_fragment_names_the_node_by_its_path_at_the_keys_its_schema_marks() {
    let flattening =
        flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
    assert_eq!(
        flattening.graph_the_runtime_runs.nodes[3].config,
        serde_json::json!({
            "reads_radius_of": {"node": "look/soft/blur"},
            "label_shaped_like_a_reference": {"node": "blur"},
        })
    );
}

#[test]
fn a_marked_node_reference_naming_a_node_outside_the_fragment_is_left_as_written() {
    let mut graph = a_camera_through_look_holding_soft_into_a_display();
    graph["nodes"][1]["fragment"]["nodes"][1]["fragment"]["nodes"][2]["config"]["reads_radius_of"] =
        serde_json::json!({"node": "camera"});
    let flattening = flattened(graph).expect("flattens");
    let tint = flattening
        .graph_the_runtime_runs
        .nodes
        .iter()
        .find(|node| node.name == "look/soft/tint")
        .expect("tint runs");
    assert_eq!(
        tint.config["reads_radius_of"],
        serde_json::json!({"node": "camera"})
    );
}

#[test]
fn a_set_variable_inside_a_fragment_names_its_referenced_node_by_its_path() {
    let mut graph = a_camera_through_look_holding_soft_into_a_display();
    graph["nodes"][1]["fragment"]["nodes"][1]["fragment"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "set-blur", "type": SET_VARIABLE_NODE_TYPE,
            "config": {"variable": "blur", "node_reference": {"node": "blur"}},
        }));
    let flattening = flattened(graph).expect("flattens");
    let setter = flattening
        .graph_the_runtime_runs
        .nodes
        .iter()
        .find(|node| node.name == "look/soft/set-blur")
        .expect("the setter is kept");
    assert_eq!(
        setter.config,
        serde_json::json!({"variable": "blur", "node_reference": {"node": "look/soft/blur"}})
    );
}

#[test]
fn a_link_to_a_port_the_fragment_does_not_declare_is_refused_naming_the_ports_it_does() {
    let mut graph = a_camera_through_look_holding_soft_into_a_display();
    graph["links"][0]["target"]["port"] = serde_json::json!("audio");
    let refusal = flattened(graph).expect_err("refused").to_string();
    assert!(
        refusal.contains(
            "a link enters fragment `look` at `audio`, which its graph generator does not \
                 declare as an Input; its Inputs are: `video`"
        ),
        "{refusal}"
    );
}

#[test]
fn a_graph_holding_no_fragment_flattens_to_itself() {
    let graph = serde_json::json!({
        "nodes": [{"name": "camera", "type": "tatolab.camera:CameraSource"}],
    });
    let flattening = flattened(graph.clone()).expect("flattens");
    assert_eq!(
        flattening.graph_the_runtime_runs,
        GraphSnapshot::from_graph_document(graph).unwrap()
    );
    assert_eq!(
        flattening.fragments_as_drawn,
        GraphFragmentsAsDrawn::default()
    );
}

#[test]
fn a_key_inside_a_fragment_the_runtime_does_not_read_is_refused_naming_its_path() {
    let mut graph = a_camera_through_look_holding_soft_into_a_display();
    graph["nodes"][1]["fragment"]["nodes"][1]["fragment"]["nodes"][1]["colour"] =
        serde_json::json!("red");
    let refusal = flattened(graph).expect_err("refused").to_string();
    assert!(
        refusal.contains("node `look/soft/blur` carries the key `colour`"),
        "{refusal}"
    );
}

#[test]
fn a_fragment_written_as_its_graph_document_reads_back_unchanged() {
    let graph =
        GraphSnapshot::from_graph_document(a_camera_through_look_holding_soft_into_a_display())
            .unwrap()
            .with_a_uuid_minted_for_each_node_holding_none();
    let graph = GraphSnapshot {
        asset: Some(uuid::Uuid::new_v4()),
        ..graph
    };
    let document: serde_json::Value =
        serde_json::from_slice(&graph.graph_document_bytes().unwrap()).unwrap();
    assert_eq!(
        document["nodes"][1]["fragment"]["nodes"][1]["fragment"]["links"][1]["source"]["node"],
        serde_json::json!(BLUR_UUID)
    );
    assert_eq!(GraphSnapshot::from_graph_document(document).unwrap(), graph);
}

fn rendered_as_running(node: &GraphSnapshotNode) -> ProcessorNodeOutput {
    ProcessorNodeOutput {
        components: serde_json::Map::from_iter([("running".to_string(), serde_json::json!(true))]),
        ..rendered_as_stored(node)
    }
}

fn a_render_of(graph: &GraphSnapshot) -> GraphResponse {
    GraphResponse {
        asset: None,
        display_name: None,
        stream: None,
        clock: Default::default(),
        nodes: graph.nodes.iter().map(rendered_as_running).collect(),
        links: graph
            .links
            .iter()
            .map(|link| LinkOutput {
                id: format!("{}->{}", link.source.node, link.target.node),
                state: LinkStateOutput::Wired,
                ..rendered_as_drawn(link)
            })
            .collect(),
        references: Vec::new(),
        machine: "rig".to_string(),
        groups: Vec::new(),
        studio: None,
    }
}

#[test]
fn graph_renders_each_fragment_as_a_container_inside_its_container_that_reads_back() {
    let flattening =
        flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
    let mut render = a_render_of(&flattening.graph_the_runtime_runs);
    flattening.fragments_as_drawn.render_into(&mut render);

    let names: Vec<&str> = render.nodes.iter().map(|node| node.name.as_str()).collect();
    assert_eq!(names, ["camera", "look", "display"]);
    let look = &render.nodes[1];
    assert_eq!(look.kind, Some(NodeKindOutput::Fragment));
    assert_eq!(look.id, LOOK_UUID);
    assert_eq!(look.ports.inputs[0].name, "video");
    assert_eq!(look.ports.outputs[0].name, "video");
    let look_holds = look.fragment.as_ref().expect("look holds its fragment");
    let names: Vec<&str> = look_holds
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(names, ["source", "soft", "out"]);
    let soft = &look_holds.nodes[1];
    assert_eq!(soft.kind, Some(NodeKindOutput::Fragment));
    let soft_holds = soft.fragment.as_ref().expect("soft holds its fragment");
    let names: Vec<&str> = soft_holds
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(names, ["source", "blur", "tint", "out"]);
    assert_eq!(
        soft_holds.nodes[1].components["running"],
        serde_json::json!(true)
    );
    let blur_into_tint = &soft_holds.links[1];
    assert_eq!(
        (
            blur_into_tint.source.node.as_str(),
            blur_into_tint.target.node.as_str()
        ),
        ("blur", "tint")
    );
    assert_eq!(blur_into_tint.state, LinkStateOutput::Wired);
    assert_eq!(
        soft_holds.nodes[2].config,
        Some(serde_json::json!({
            "reads_radius_of": {"node": "blur"},
            "label_shaped_like_a_reference": {"node": "blur"},
        }))
    );
    let outer_links: Vec<(&str, &str)> = render
        .links
        .iter()
        .map(|link| (link.source.node.as_str(), link.target.node.as_str()))
        .collect();
    assert_eq!(outer_links, [("camera", "look"), ("look", "display")]);

    let read_back = GraphSnapshot::from_graph_document(serde_json::to_value(&render).unwrap())
        .expect("the render loads");
    let written =
        GraphSnapshot::from_graph_document(a_camera_through_look_holding_soft_into_a_display())
            .unwrap();
    assert_eq!(
        read_back
            .with_graph_fragments_flattened(
                &node_types_describing_a_tint_that_reads_a_radius_by_reference()
            )
            .unwrap()
            .graph_the_runtime_runs,
        written
            .with_graph_fragments_flattened(
                &node_types_describing_a_tint_that_reads_a_radius_by_reference()
            )
            .unwrap()
            .graph_the_runtime_runs
    );
}

fn ids_and_states_of(links: &[LinkOutput]) -> Vec<(&str, LinkStateOutput)> {
    links
        .iter()
        .map(|link| (link.id.as_str(), link.state))
        .collect()
}

#[test]
fn each_drawn_link_through_a_fragment_renders_the_running_link_it_stands_for() {
    let flattening =
        flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
    let mut render = a_render_of(&flattening.graph_the_runtime_runs);
    flattening.fragments_as_drawn.render_into(&mut render);

    let camera_into_blur = ("camera->look/soft/blur", LinkStateOutput::Wired);
    let tint_into_display = ("look/soft/tint->display", LinkStateOutput::Wired);
    assert_eq!(
        ids_and_states_of(&render.links),
        [camera_into_blur, tint_into_display]
    );
    let look_holds = render.nodes[1].fragment.as_ref().expect("look holds");
    assert_eq!(
        ids_and_states_of(&look_holds.links),
        [camera_into_blur, tint_into_display]
    );
    let soft_holds = look_holds.nodes[1].fragment.as_ref().expect("soft holds");
    assert_eq!(
        ids_and_states_of(&soft_holds.links),
        [
            camera_into_blur,
            ("look/soft/blur->look/soft/tint", LinkStateOutput::Wired),
            tint_into_display,
        ]
    );
    let outer_ends: Vec<(&str, &str)> = render
        .links
        .iter()
        .map(|link| (link.source.node.as_str(), link.target.node.as_str()))
        .collect();
    assert_eq!(outer_ends, [("camera", "look"), ("look", "display")]);
}

#[test]
fn a_drawn_link_into_a_fragment_renders_pending_while_the_link_it_stands_for_is() {
    let flattening =
        flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
    let mut render = a_render_of(&flattening.graph_the_runtime_runs);
    for link in &mut render.links {
        if link.id == "camera->look/soft/blur" {
            link.state = LinkStateOutput::Pending;
        }
    }
    flattening.fragments_as_drawn.render_into(&mut render);

    assert_eq!(
        ids_and_states_of(&render.links),
        [
            ("camera->look/soft/blur", LinkStateOutput::Pending),
            ("look/soft/tint->display", LinkStateOutput::Wired),
        ]
    );
}

#[test]
fn a_drawn_link_into_a_fragment_port_nothing_runs_behind_renders_with_its_ends_alone() {
    let flattening =
        flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
    let mut render = a_render_of(&flattening.graph_the_runtime_runs);
    render
        .links
        .retain(|link| link.id != "camera->look/soft/blur");
    flattening.fragments_as_drawn.render_into(&mut render);

    let camera_into_look = &render.links[0];
    assert_eq!(camera_into_look.id, "");
    assert_eq!(camera_into_look.state, LinkStateOutput::Pending);
}

#[test]
fn two_fragments_of_one_name_on_one_level_are_refused_naming_it() {
    let mut graph = a_camera_through_look_holding_soft_into_a_display();
    let second_look = serde_json::json!({
        "name": "Look", "type": "looks.generators:look",
        "fragment": {"nodes": [], "links": []},
    });
    graph["nodes"].as_array_mut().unwrap().push(second_look);
    let refusal = flattened(graph).expect_err("refused");
    assert!(
        matches!(&refusal, Error::NodeNameTaken { cast, .. } if cast == "look"),
        "{refusal}"
    );
}

#[test]
fn a_node_inside_a_fragment_is_read_only_to_a_live_removal_or_rename() {
    let flattening =
        flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
    let drawn = crate::core::graph_snapshot::GraphOnlyNodesAsDrawn {
        fragments_as_drawn: flattening.fragments_as_drawn,
        ..Default::default()
    };
    let refusal = drawn
        .refuse_a_node_change_a_drawn_link_names(
            "look/soft/blur",
            crate::core::graph_snapshot::NodeChangeADrawnLinkBlocks::Removal,
        )
        .expect_err("refused")
        .to_string();
    assert!(
        refusal.contains("it is part of fragment `look/soft`, which its graph generator built"),
        "{refusal}"
    );
}
