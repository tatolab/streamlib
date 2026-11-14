// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pin value each helper's unlinked Channels input reads: the one set on
//! its node, else the one its port declares.

use crate::core::descriptors::{NodeKind, PortFamily};
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, OutOfProcessLinkWiringComponent, ProcessorNode,
};
use serde_json::Value as JsonValue;

use super::function_node_island::runs_inside_the_read_of_each_node_it_feeds;

/// Hand each helper the pin values its input ports read while unlinked, when
/// they differ from those it holds.
pub(crate) fn hand_each_helper_the_pin_values_its_input_ports_read(graph: &Graph) -> Result<()> {
    for node in graph.traversal().v(()).iter() {
        if runs_inside_the_read_of_each_node_it_feeds(graph, &node.id) {
            continue;
        }
        let Some(link_wiring) = node.get::<OutOfProcessLinkWiringComponent>() else {
            continue;
        };
        link_wiring
            .0
            .record_the_pin_values_and_hand_them_to_a_far_side_past_its_setup_command(
                the_pin_values_its_input_ports_read(node),
            )?;
    }
    Ok(())
}

/// Hand each helper the config fields its node takes as inputs, when they
/// differ from those it holds.
pub(crate) fn hand_each_helper_the_config_fields_its_node_exposes_as_inputs(
    graph: &Graph,
) -> Result<()> {
    for node in graph.traversal().v(()).iter() {
        if runs_inside_the_read_of_each_node_it_feeds(graph, &node.id) {
            continue;
        }
        let Some(link_wiring) = node.get::<OutOfProcessLinkWiringComponent>() else {
            continue;
        };
        link_wiring
            .0
            .record_the_exposed_config_fields_and_hand_them_to_a_far_side_past_its_setup_command(
                node.exposed_config_fields.clone(),
            )?;
    }
    Ok(())
}

/// The value each Channels input of `node` reads while unlinked, by port: the
/// pin value set on it, else its declared default, as a number.
fn the_pin_values_its_input_ports_read(node: &ProcessorNode) -> serde_json::Map<String, JsonValue> {
    node.ports
        .inputs
        .iter()
        .filter(|port| port.family.family == PortFamily::Channels)
        .filter_map(|port| {
            let pin_value = node.pins.get(&port.name).or(port.pin_default.as_ref())?;
            the_control_value_of_a_pin(pin_value)
                .map(|control_value| (port.name.clone(), JsonValue::from(control_value)))
        })
        .collect()
}

fn the_control_value_of_a_pin(pin_value: &JsonValue) -> Option<f64> {
    match pin_value {
        JsonValue::Bool(pin_is_on) => Some(if *pin_is_on { 1.0 } else { 0.0 }),
        JsonValue::Number(number) => number.as_f64(),
        _ => None,
    }
}

/// The inputs of function node `node` in `graph` that no link feeds, that carry
/// no pin value and that declare no default, so the node has no value to
/// evaluate them with; none for a node of any other kind.
pub(crate) fn the_inputs_a_function_node_has_no_value_for(
    graph: &Graph,
    node: &ProcessorNode,
) -> Vec<String> {
    if !node
        .node_kind()
        .is_some_and(|node_kind| matches!(node_kind, NodeKind::Function { .. }))
    {
        return Vec::new();
    }
    let linked_input_ports: Vec<String> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| link.to_port().processor_id() == &node.id)
        .map(|link| link.to_port().port_name().to_string())
        .collect();
    node.ports
        .inputs
        .iter()
        .filter(|port| {
            port.pin_default.is_none()
                && !node.is_an_exposed_config_field_input(&port.name)
                && !node.pins.contains_key(&port.name)
                && !linked_input_ports.contains(&port.name)
        })
        .map(|port| port.name.clone())
        .collect()
}

/// Refuse a stream about to start whose function nodes have an input with no
/// value, naming each such input on its node.
pub(crate) fn refuse_a_function_node_input_with_no_value_as_the_stream_starts(
    graph: &Graph,
) -> Result<()> {
    let inputs_with_no_value: Vec<String> = graph
        .traversal()
        .v(())
        .iter()
        .flat_map(|node| {
            the_inputs_a_function_node_has_no_value_for(graph, node)
                .into_iter()
                .map(|input| format!("`{}`'s input `{input}`", node.display_name))
        })
        .collect();
    if inputs_with_no_value.is_empty() {
        return Ok(());
    }
    Err(Error::InvalidGraph(format!(
        "the stream does not start: no value reaches {}, each linked to nothing, with no pin \
         value and declaring no default. To fix: link each, set its pin, or give its \
         parameter a default",
        inputs_with_no_value.join(", ")
    )))
}

/// Refuse a pin value set on an input of a node that is not a function node
/// unless it is a number or a bool on a Channels input, which is all such a
/// node's read carries.
pub(crate) fn refuse_a_pin_value_a_process_node_cannot_read(
    node: &ProcessorNode,
    input: &str,
    pin_value: &JsonValue,
) -> Result<()> {
    if node
        .node_kind()
        .is_some_and(|node_kind| matches!(node_kind, NodeKind::Function { .. }))
    {
        return Ok(());
    }
    let Some(port) = node.ports.inputs.iter().find(|port| port.name == input) else {
        return Ok(());
    };
    if port.family.family != PortFamily::Channels {
        return Err(Error::Configuration(format!(
            "input `{input}` of `{}` carries {}, and a pin value is a control value, which only \
             a Channels input reads. To fix: link `{input}` instead",
            node.display_name, port.family.family
        )));
    }
    if the_control_value_of_a_pin(pin_value).is_none() {
        return Err(Error::Configuration(format!(
            "the pin value of input `{input}` of `{}` is {pin_value}, and a Channels input reads \
             a number or a bool",
            node.display_name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::descriptors::{
        PortDescriptor, PortFamilyDeclaration, ProcessorClassImportPath, ProcessorClassShortName,
        ProcessorDescriptor,
    };
    use crate::core::execution::ProcessExecution;
    use crate::core::graph::GraphNodeWithComponents;
    use crate::core::processors::{
        HowANodeOfADescribedTypeRuns, NodeTypesOneStreamResolves, OutOfProcessLinkWiringEnvelope,
        ProcessorSpec,
    };
    use crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery;
    use crate::core::{ProcessorUniqueId, descriptors::PortFamily};

    const LEVEL_METER_NODE_TYPE: &str = "test_nodes:LevelMeter";

    /// A graph resolving a process node type reading `level`, which declares
    /// a default of 0.5, `gain`, which declares none, and `frames`, Records.
    fn a_graph_resolving_a_process_node_type_with_channels_inputs() -> Graph {
        let import_path = ProcessorClassImportPath::new(LEVEL_METER_NODE_TYPE).unwrap();
        let descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new("LevelMeter").unwrap(),
            import_path,
            "a process node type for a test",
        )
        .with_input(
            PortDescriptor::new(
                "level",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            )
            .with_pin_default(serde_json::json!(0.5)),
        )
        .with_input(PortDescriptor::new(
            "gain",
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Channels),
        ))
        .with_input(PortDescriptor::new(
            "frames",
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Records),
        ));
        let node_types = NodeTypesOneStreamResolves::new();
        node_types
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor,
                HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no processor in this test",
                        node.id
                    )))
                })),
            )
            .unwrap();
        crate::core::test_support::ensure_test_mocks_registered();
        Graph::new_resolving_node_types_through(Arc::new(node_types))
    }

    fn a_level_meter_in_a_helper(graph: &mut Graph) -> ProcessorUniqueId {
        let level_meter = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                ProcessorClassImportPath::new(LEVEL_METER_NODE_TYPE).unwrap(),
                JsonValue::Null,
            ))
            .unwrap()
            .first()
            .unwrap()
            .id
            .clone();
        graph
            .traversal_mut()
            .v(&level_meter)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
                OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    ProcessExecution::Continuous { interval_ms: 10 },
                ),
            )));
        level_meter
    }

    fn the_link_wiring_of(
        graph: &Graph,
        node_id: &ProcessorUniqueId,
    ) -> Arc<OutOfProcessLinkWiringEnvelope> {
        Arc::clone(
            &graph
                .traversal()
                .v(node_id)
                .first()
                .unwrap()
                .get::<OutOfProcessLinkWiringComponent>()
                .unwrap()
                .0,
        )
    }

    fn set_a_pin(graph: &mut Graph, node_id: &ProcessorUniqueId, input: &str, pin: JsonValue) {
        graph
            .traversal_mut()
            .v(node_id)
            .first_mut()
            .unwrap()
            .pins
            .insert(input.to_string(), pin);
    }

    #[test]
    fn a_helpers_setup_carries_each_declared_default_and_a_pin_edit_after_it_is_handed_over() {
        let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
        let level_meter = a_level_meter_in_a_helper(&mut graph);
        hand_each_helper_the_pin_values_its_input_ports_read(&graph).unwrap();
        assert_eq!(
            the_link_wiring_of(&graph, &level_meter).as_setup_command_ports()["pins"],
            serde_json::json!({"level": 0.5}),
            "an input declaring a default reads it, and one declaring none reads only its links"
        );

        let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
        the_link_wiring_of(&graph, &level_meter)
            .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
            .unwrap();
        set_a_pin(&mut graph, &level_meter, "gain", serde_json::json!(2));
        set_a_pin(&mut graph, &level_meter, "level", serde_json::json!(true));
        hand_each_helper_the_pin_values_its_input_ports_read(&graph).unwrap();
        hand_each_helper_the_pin_values_its_input_ports_read(&graph).unwrap();
        assert_eq!(
            *delivery.pin_values_handed_over.lock(),
            vec![
                serde_json::json!({"gain": 2.0, "level": 1.0})
                    .as_object()
                    .unwrap()
                    .clone()
            ],
            "a pin set wins over the default, a bool reads as 1 or 0, and pin values that did \
             not change are not handed over again"
        );
    }

    #[test]
    fn a_helpers_setup_carries_the_config_fields_its_node_exposes_and_a_later_change_is_handed_over()
     {
        let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
        let level_meter = a_level_meter_in_a_helper(&mut graph);
        let expose = |graph: &mut Graph, exposed_config_fields: &[&str]| {
            graph
                .traversal_mut()
                .v(&level_meter)
                .first_mut()
                .unwrap()
                .exposed_config_fields = exposed_config_fields
                .iter()
                .map(ToString::to_string)
                .collect();
        };
        expose(&mut graph, &["threshold"]);
        hand_each_helper_the_config_fields_its_node_exposes_as_inputs(&graph).unwrap();
        assert_eq!(
            the_link_wiring_of(&graph, &level_meter).as_setup_command_ports()["exposed_config_fields"],
            serde_json::json!(["threshold"])
        );

        let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
        the_link_wiring_of(&graph, &level_meter)
            .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
            .unwrap();
        expose(&mut graph, &["threshold", "ceiling"]);
        hand_each_helper_the_config_fields_its_node_exposes_as_inputs(&graph).unwrap();
        hand_each_helper_the_config_fields_its_node_exposes_as_inputs(&graph).unwrap();
        assert_eq!(
            *delivery.exposed_config_fields_handed_over.lock(),
            vec![vec!["threshold".to_string(), "ceiling".to_string()]],
            "fields that did not change are not handed over again"
        );
    }

    #[test]
    fn a_pin_value_off_a_channels_input_or_not_a_number_is_refused_naming_the_input() {
        let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
        let level_meter = a_level_meter_in_a_helper(&mut graph);
        let node = graph.traversal().v(&level_meter).first().unwrap();
        let refusal =
            refuse_a_pin_value_a_process_node_cannot_read(node, "frames", &serde_json::json!(1.0))
                .unwrap_err()
                .to_string();
        assert!(
            refusal.contains("`frames`") && refusal.contains("Channels"),
            "got {refusal}"
        );
        let refusal =
            refuse_a_pin_value_a_process_node_cannot_read(node, "gain", &serde_json::json!("loud"))
                .unwrap_err()
                .to_string();
        assert!(
            refusal.contains("`gain`") && refusal.contains("a number or a bool"),
            "got {refusal}"
        );
        refuse_a_pin_value_a_process_node_cannot_read(node, "gain", &serde_json::json!(0.25))
            .unwrap();
    }

    /// A graph holding `remap`, whose `x` declares no default, and `mix`,
    /// whose `b` declares a default of 0.5, with `remap` reading `mix`'s
    /// `result` and `mix`'s `a` linked to nothing.
    fn a_graph_of_two_function_nodes_with_inputs_left_unlinked()
    -> (Graph, ProcessorUniqueId, ProcessorUniqueId) {
        use crate::core::compiler::compiler_ops::function_node_island::tests::{
            a_function_node_type, a_graph_resolving_these_function_node_types, add_a_node, link,
        };
        let mut mix = a_function_node_type("test_ops:mix", &["a"], &["result"]);
        mix = mix.with_input(
            PortDescriptor::new(
                "b",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            )
            .with_pin_default(serde_json::json!(0.5)),
        );
        let mut graph = a_graph_resolving_these_function_node_types([
            a_function_node_type("test_ops:remap", &["x", "gain"], &["result"]),
            mix,
        ]);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        link(&mut graph, (&mix, "result"), (&remap, "x"));
        (graph, remap, mix)
    }

    #[test]
    fn a_function_node_input_with_no_link_pin_or_default_is_named_and_refused_as_the_stream_starts()
    {
        let (mut graph, remap, mix) = a_graph_of_two_function_nodes_with_inputs_left_unlinked();
        let waiting = |graph: &Graph, node_id: &ProcessorUniqueId| {
            the_inputs_a_function_node_has_no_value_for(
                graph,
                graph.traversal().v(node_id).first().unwrap(),
            )
        };
        assert_eq!(waiting(&graph, &remap), vec!["gain".to_string()]);
        assert_eq!(
            waiting(&graph, &mix),
            vec!["a".to_string()],
            "an input declaring a default has a value"
        );
        let refusal = refuse_a_function_node_input_with_no_value_as_the_stream_starts(&graph)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("`gain`") && refusal.contains("`a`"),
            "the refusal names each input with no value, got {refusal}"
        );

        set_a_pin(&mut graph, &remap, "gain", serde_json::json!(2.0));
        set_a_pin(&mut graph, &mix, "a", serde_json::json!(0.25));
        assert!(waiting(&graph, &remap).is_empty() && waiting(&graph, &mix).is_empty());
        refuse_a_function_node_input_with_no_value_as_the_stream_starts(&graph).unwrap();
    }

    #[test]
    fn a_process_node_input_left_unlinked_never_waits_for_a_value() {
        let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
        let level_meter = a_level_meter_in_a_helper(&mut graph);
        assert!(
            the_inputs_a_function_node_has_no_value_for(
                &graph,
                graph.traversal().v(&level_meter).first().unwrap()
            )
            .is_empty()
        );
        refuse_a_function_node_input_with_no_value_as_the_stream_starts(&graph).unwrap();
    }

    #[test]
    fn the_graph_names_the_inputs_a_function_node_waits_on() {
        let (graph, _remap, _mix) = a_graph_of_two_function_nodes_with_inputs_left_unlinked();
        let rendered =
            serde_json::to_value(graph.to_graph_response("this-machine".to_string())).unwrap();
        let waiting_by_node_type: std::collections::HashMap<String, JsonValue> = rendered["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| {
                (
                    node["type"].as_str().unwrap().to_string(),
                    node["inputs_waiting_for_a_value"].clone(),
                )
            })
            .collect();
        assert_eq!(
            waiting_by_node_type["test_ops:remap"],
            serde_json::json!(["gain"])
        );
        assert_eq!(
            waiting_by_node_type["test_ops:mix"],
            serde_json::json!(["a"])
        );
    }
}
