// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::super::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_VALUE_PORT_NAME, GraphInterfaceLevel,
    GraphInterfacePortDirection,
};
use crate::core::descriptors::{PinAnnotations, PinNumberRange, PinType};
use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::graph::{GraphNode, PortInfo};
use crate::core::json_schema::{GraphNodePortOnTheWire, LinkPortRefOutput};

/// A `GraphInput` node saved with `settings`, its ports resolved against them.
fn a_graph_input_saved_with(settings: serde_json::Value) -> GraphNode {
    let descriptor =
        crate::core::nodes::the_node_type_of_a_graph_input().expect("the GraphInput type builds");
    GraphNode::new(
        descriptor.node_class_import_path.clone(),
        None,
        "lines",
        settings.as_object().cloned().unwrap_or_default(),
        descriptor.inputs.iter().map(PortInfo::from).collect(),
        descriptor.outputs.iter().map(PortInfo::from).collect(),
    )
}

/// The `value` input of `node`, as `graph` renders it.
fn the_value_input_rendered(node: &GraphNode) -> serde_json::Value {
    serde_json::to_value(GraphNodePortOnTheWire::from(
        node.input_port(GRAPH_INTERFACE_VALUE_PORT_NAME)
            .expect("a GraphInput has a `value` input"),
    ))
    .expect("the port renders")
}

/// A `GraphInput` choosing an Integer, saved with a range, a step, a unit, a meaning and a
/// control, carries them as the annotations of its `value` input, which `graph` renders on it,
/// and checks a value set on it against the range.
///
/// Mental-revert: without resolving the settings onto the `value` input, it carries the empty
/// annotations its node type declares and `graph` renders none.
#[test]
fn a_graph_input_carries_the_annotations_its_settings_state_on_its_value_input() {
    let node = a_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "integer"}, "value": 2, "range": [1, 8],
        "step": 1, "unit": "lines", "meaning": "count", "control": "dial",
    }));

    let value = node
        .input_port(GRAPH_INTERFACE_VALUE_PORT_NAME)
        .expect("a GraphInput has a `value` input");
    assert_eq!(
        value.annotations.0,
        PinAnnotations {
            range: Some(PinNumberRange {
                minimum: Some(1.0),
                maximum: Some(8.0),
            }),
            step: Some(1.0),
            unit: Some("lines".to_owned()),
            meaning: Some("count".to_owned()),
            control: Some("dial".to_owned()),
            ..PinAnnotations::default()
        }
    );
    let rendered = the_value_input_rendered(&node);
    assert_eq!(
        rendered["annotations"],
        json!({
            "range": {"minimum": 1.0, "maximum": 8.0}, "step": 1.0, "unit": "lines",
            "meaning": "count", "control": "dial",
        }),
        "{rendered}"
    );
}

/// A `GraphInput` saved before its value had annotations, with a pin type and a value alone,
/// renders its `value` input with none.
///
/// Mental-revert: rendering the annotations whether or not any is stated serves an empty
/// `annotations` on every port.
#[test]
fn a_graph_input_saved_before_annotations_renders_its_value_input_with_none() {
    let node = a_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "float"}, "value": 2,
    }));

    let rendered = the_value_input_rendered(&node);
    assert!(rendered.get("annotations").is_none(), "{rendered}");
    assert!(
        node.input_port(GRAPH_INTERFACE_VALUE_PORT_NAME)
            .is_some_and(|value| value.annotations.0.is_empty())
    );
}

fn the_pin_type_no_linked_port_states(
    _linked_port: &LinkPortRefOutput,
    _direction: GraphInterfacePortDirection,
) -> Option<PinType> {
    None
}

/// The refusal, in words, of a stream whose top level holds one `GraphInput` saved with
/// `settings`.
fn the_refusal_of_a_top_level_graph_input_saved_with(settings: serde_json::Value) -> String {
    GraphSnapshot::from_graph_document(json!({
        "nodes": [{"name": "lines", "type": GRAPH_INPUT_NODE_TYPE, "input_values": settings}],
        "links": [],
    }))
    .and_then(|graph| {
        graph.with_graph_interface_resolved(
            GraphInterfaceLevel::TheStreamsTopLevel,
            &the_pin_type_no_linked_port_states,
        )
    })
    .expect_err("the Graph Input is refused")
    .to_string()
}

/// An annotation the chosen type does not take is refused naming it, the type and the
/// annotations the type takes; one stated with no type chosen is refused naming `pin_type`; and
/// a step or a control of another form is refused naming it.
///
/// Mental-revert: without checking each annotation against the chosen type's row, a Boolean
/// Input takes a range and a unit, and a String one a step.
#[test]
fn an_annotation_the_chosen_type_does_not_take_is_refused_naming_it() {
    let unit_on_a_boolean = the_refusal_of_a_top_level_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "boolean"}, "unit": "Hz",
    }));
    assert!(
        unit_on_a_boolean.contains("states `unit`")
            && unit_on_a_boolean.contains("a Boolean pin does not take")
            && unit_on_a_boolean.contains("it takes `meaning`, `control`"),
        "{unit_on_a_boolean}"
    );
    let range_on_a_boolean = the_refusal_of_a_top_level_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "boolean"}, "range": [0, 1],
    }));
    assert!(
        range_on_a_boolean.contains("states `range`"),
        "{range_on_a_boolean}"
    );
    let step_on_a_string = the_refusal_of_a_top_level_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "string"}, "step": 1,
    }));
    assert!(
        step_on_a_string.contains("states `step`") && step_on_a_string.contains("String"),
        "{step_on_a_string}"
    );
    let control_with_no_type = the_refusal_of_a_top_level_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "float"}, "control": "dial",
    }));
    assert!(
        control_with_no_type.contains("states `control`")
            && control_with_no_type.contains("chooses no `pin_type`"),
        "{control_with_no_type}"
    );
    let a_step_of_zero = the_refusal_of_a_top_level_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "float"}, "step": 0,
    }));
    assert!(
        a_step_of_zero.contains("its `step` as 0"),
        "{a_step_of_zero}"
    );
    let a_control_by_number = the_refusal_of_a_top_level_graph_input_saved_with(json!({
        "port": "lines", "pin_type": {"type": "float"}, "control": 3,
    }));
    assert!(
        a_control_by_number.contains("its `control` as 3"),
        "{a_control_by_number}"
    );
}
