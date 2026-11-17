// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A port a node type renamed is found under each name it had, and what the graph holds, reads
//! and answers names it by its current name: a node's values, a link's ends, a graph document read
//! in, and the check of a value set on it. An input that may be unset takes no value at all.

use serde_json::{Map, Value, json};

use super::graph_snapshot::GraphSnapshot;
use super::input_values::{
    InputValueRefusalCode, NodeAnInputValueIsSetOn, refuse_an_input_value_its_input_does_not_take,
};
use super::{Graph, InputLinkPortRef, OutputLinkPortRef};
use crate::core::descriptors::NodeClassImportPath;
use crate::core::nodes::NODE_REGISTRY;
use crate::core::nodes::{NodeSpec, NodeTypesOneStreamResolves};
use crate::core::test_support::{MockPinTypedNode, ensure_test_mocks_registered};

/// Mock node whose ports were renamed: its input `gain` was `level`, and may be unset; its input
/// `picture` was `video_in`; its output `frames` was `video`.
#[crate::node(
    execution = reactive,
    input("gain", pin_type = Float, former_names = ["level"], may_be_unset = true),
    input("picture", pin_type = Texture, former_names = ["video_in"]),
    output("frames", pin_type = Texture, former_names = ["video"]),
)]
struct MockRenamedPortsNode;

impl crate::core::ReactiveNode for MockRenamedPortsNode::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Register the test mocks and [`MockRenamedPortsNode`], once.
fn ensure_the_renamed_ports_node_registered() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    ensure_test_mocks_registered();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<MockRenamedPortsNode::Node>());
}

/// Add a node of `node_type` named `name`, holding `pins`, to `graph`, answering its run id.
fn add_a_node_holding(
    graph: &mut Graph,
    node_type: NodeClassImportPath,
    name: &str,
    pins: Value,
) -> String {
    let Value::Object(pins) = pins else {
        panic!("the pins are an object");
    };
    graph
        .traversal_mut()
        .add_v(
            NodeSpec::new(node_type, json!({}))
                .with_display_name(name)
                .with_pins_and_studio_fields(pins, Map::new()),
        )
        .expect("the node is added")
        .first()
        .expect("the node is held")
        .id
        .to_string()
}

/// A value saved under a name an input had lands on the input under its name, beside a value
/// saved under its name, which stays; a link to and from a port under a name it had joins the
/// ports under their names, and the node answers the port by either name.
///
/// Mental-revert: holding the pins as given leaves `level` on the node and `gain` reading
/// nothing; finding a port by exact name alone adds no link to `video_in`.
#[test]
fn a_value_and_a_link_under_a_name_a_port_had_land_on_the_port_under_its_name() {
    ensure_the_renamed_ports_node_registered();
    let mut graph = Graph::new();
    let renamed = add_a_node_holding(
        &mut graph,
        MockRenamedPortsNode::node_class_import_path(),
        "mixer",
        json!({ "level": 0.25 }),
    );
    let held_twice = add_a_node_holding(
        &mut graph,
        MockRenamedPortsNode::node_class_import_path(),
        "mixer-2",
        json!({ "level": 0.25, "gain": 0.75 }),
    );
    let source = add_a_node_holding(
        &mut graph,
        MockPinTypedNode::node_class_import_path(),
        "source",
        json!({}),
    );

    let pins_of = |graph: &Graph, node: &str| {
        graph
            .traversal()
            .v(node)
            .first()
            .expect("the node is held")
            .pins
            .clone()
    };
    assert_eq!(
        Value::Object(pins_of(&graph, &renamed)),
        json!({ "gain": 0.25 })
    );
    assert_eq!(
        Value::Object(pins_of(&graph, &held_twice)),
        json!({ "gain": 0.75 })
    );

    let into_the_former_name = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(source.as_str(), "frames"),
            InputLinkPortRef::new(renamed.as_str(), "video_in"),
        )
        .first()
        .map(|link| link.to_port().port_name().to_owned());
    assert_eq!(into_the_former_name.as_deref(), Some("picture"));
    let out_of_the_former_name = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(renamed.as_str(), "video"),
            InputLinkPortRef::new(held_twice.as_str(), "picture"),
        )
        .first()
        .map(|link| link.source().port_name().to_owned());
    assert_eq!(out_of_the_former_name.as_deref(), Some("frames"));

    let node = graph.traversal().v(renamed.as_str()).first().unwrap();
    assert_eq!(node.input("level").port_name(), "gain");
    assert_eq!(node.output("video").port_name(), "frames");
    assert!(node.try_input("loudness").is_none());
}

/// A graph document whose values and links name ports by names they had reads with every one
/// under its port's current name, so what validates, loads and reconciles after it knows only
/// the current names.
///
/// Mental-revert: dropping the step that moves them to the current names leaves `level` among
/// the mixer's values and the link naming `video_in`, which validation refuses.
#[test]
fn a_graph_document_naming_ports_by_names_they_had_reads_under_their_names() {
    ensure_the_renamed_ports_node_registered();
    let document = json!({
        "nodes": [
            { "name": "source", "type": MockPinTypedNode::node_class_import_path().as_str(),
              "config": {} },
            { "name": "mixer", "type": MockRenamedPortsNode::node_class_import_path().as_str(),
              "config": {}, "pins": { "level": 0.5 } },
            { "name": "mixer-2", "type": MockRenamedPortsNode::node_class_import_path().as_str(),
              "config": {} },
        ],
        "links": [
            { "source": { "node": "source", "port": "frames" },
              "target": { "node": "mixer", "port": "video_in" } },
            { "source": { "node": "mixer", "port": "video" },
              "target": { "node": "mixer-2", "port": "Video_In" } },
        ],
    });
    let node_types = NodeTypesOneStreamResolves::new();
    let resolution = GraphSnapshot::from_graph_document(document)
        .expect("the document reads")
        .with_graph_only_nodes_resolved(&node_types)
        .expect("its graph-only nodes resolve");
    resolution
        .validate(&node_types)
        .expect("the graph under its current port names validates");

    let graph = &resolution.graph_the_runtime_runs;
    let mixer = graph
        .nodes
        .iter()
        .find(|node| node.name == "mixer")
        .expect("the mixer is held");
    assert_eq!(
        Value::Object(mixer.input_values.clone()),
        json!({ "gain": 0.5 })
    );
    let link_ends: Vec<(&str, &str)> = graph
        .links
        .iter()
        .map(|link| (link.source.port.as_str(), link.target.port.as_str()))
        .collect();
    assert_eq!(link_ends, [("frames", "picture"), ("frames", "picture")]);
}

/// The refusal, if any, of `value` set on input `input` of a node named `mixer` of `node_type`.
fn the_refusal_of(
    node_type: &NodeClassImportPath,
    input: &str,
    value: Value,
) -> Result<(), super::input_values::InputValueRefusal> {
    ensure_the_renamed_ports_node_registered();
    let input_values_after_the_set = Map::from_iter([(input.to_owned(), value.clone())]);
    refuse_an_input_value_its_input_does_not_take(
        &NodeTypesOneStreamResolves::new(),
        &NodeAnInputValueIsSetOn {
            name: "mixer",
            node_type,
            input_values_after_the_set: &input_values_after_the_set,
        },
        input,
        Some(&value),
    )
}

/// A value set under a name an input had is checked as the input's, and refused naming the
/// input by its name; an input that may be unset takes `null`, which one that may not refuses.
///
/// Mental-revert: finding the input by exact name refuses `level` as no input; checking `null`
/// by the pin type alone refuses it on `gain`.
#[test]
fn a_value_set_under_a_name_an_input_had_is_checked_as_the_inputs_and_null_as_unset() {
    let renamed = MockRenamedPortsNode::node_class_import_path();
    assert_eq!(the_refusal_of(&renamed, "level", json!(0.5)), Ok(()));
    let refusal = the_refusal_of(&renamed, "level", json!("loud")).expect_err("text is no Float");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
    assert!(refusal.words.contains("input `gain`"), "{refusal}");

    assert_eq!(the_refusal_of(&renamed, "gain", Value::Null), Ok(()));
    let refusal = the_refusal_of(
        &MockPinTypedNode::node_class_import_path(),
        "gain",
        Value::Null,
    )
    .expect_err("an input that must hold a value refuses null");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
}
