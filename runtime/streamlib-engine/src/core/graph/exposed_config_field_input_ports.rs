// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The input port a config field takes when a node exposes it: a Channels input
//! of the field's name that a link drives.

use crate::core::descriptors::{PortFamily, PortFamilyDeclaration, ProcessorClassImportPath};
use crate::core::error::{Error, Result};
use crate::core::graph::{PortInfo, PortKind};
use crate::core::processors::NodeTypesOneStreamResolves;

/// The input port each field of `exposed_config_fields` takes on node `node_name` of type
/// `node_type`, refused naming the field when the type is natively compiled, when its config
/// schema declares no number or bool field of that name, when the field is named twice, or
/// when one of `declared_ports` already has its name.
pub(crate) fn the_input_ports_of_the_exposed_config_fields(
    node_name: &str,
    node_type: &ProcessorClassImportPath,
    node_types: &NodeTypesOneStreamResolves,
    declared_ports: &[PortInfo],
    exposed_config_fields: &[String],
) -> Result<Vec<PortInfo>> {
    if exposed_config_fields.is_empty() {
        return Ok(Vec::new());
    }
    if node_types.is_a_natively_compiled_node_type(node_type) {
        return Err(Error::Configuration(format!(
            "node `{node_name}` exposes {} as inputs, and its type `{node_type}` is natively \
             compiled, so nothing reads a config field it takes as an input. Only a Python \
             node exposes a config field",
            exposed_config_fields.join(", ")
        )));
    }
    let config_schema = node_types
        .descriptor(node_type)
        .and_then(|descriptor| descriptor.config_schema);
    let fields_it_can_expose = the_number_and_bool_fields_of(config_schema.as_ref());
    let mut input_ports: Vec<PortInfo> = Vec::with_capacity(exposed_config_fields.len());
    for field in exposed_config_fields {
        let Some((_, field_description)) = fields_it_can_expose
            .iter()
            .find(|(field_it_can_expose, _)| field_it_can_expose == field)
        else {
            return Err(Error::Configuration(format!(
                "node `{node_name}` cannot expose `{field}` as an input: its type `{node_type}` \
                 declares no number or bool config field of that name. It can expose: {}",
                if fields_it_can_expose.is_empty() {
                    "none".to_string()
                } else {
                    fields_it_can_expose
                        .iter()
                        .map(|(field_it_can_expose, _)| format!("`{field_it_can_expose}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )));
        };
        if input_ports.iter().any(|port| &port.name == field) {
            return Err(Error::Configuration(format!(
                "node `{node_name}` exposes `{field}` as an input twice"
            )));
        }
        if declared_ports.iter().any(|port| &port.name == field) {
            return Err(Error::Configuration(format!(
                "node `{node_name}` cannot expose `{field}` as an input: its type `{node_type}` \
                 already has a port of that name"
            )));
        }
        input_ports.push(PortInfo {
            name: field.clone(),
            description: field_description.clone(),
            port_kind: PortKind::default(),
            delivery_profile: Some("newest".to_string()),
            borrow_quota: None,
            audio_window: None,
            pin_default: None,
            family: PortFamilyDeclaration::of_family(PortFamily::Channels),
        });
    }
    Ok(input_ports)
}

/// Each number, integer or bool property `config_schema` declares, with its description.
fn the_number_and_bool_fields_of(
    config_schema: Option<&serde_json::Value>,
) -> Vec<(String, String)> {
    let Some(properties) = config_schema
        .and_then(|config_schema| config_schema.get("properties"))
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    properties
        .iter()
        .filter(|(_, property)| {
            matches!(
                property.get("type").and_then(serde_json::Value::as_str),
                Some("number" | "integer" | "boolean")
            )
        })
        .map(|(field, property)| {
            (
                field.clone(),
                property
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::compiler::compiler_ops::function_node_island::tests::{
        a_function_node_type, a_graph_resolving_these_function_node_types, add_a_node, link,
    };
    use crate::core::compiler::compiler_ops::function_node_island::the_island_feeding_a_link;
    use crate::core::compiler::compiler_ops::input_port_pin_values::the_inputs_a_function_node_has_no_value_for;
    use crate::core::descriptors::{PortFamily, ProcessorClassImportPath};
    use crate::core::graph::Graph;
    use crate::core::processors::ProcessorSpec;
    use crate::core::test_support::MockSourceTakingOneSetting;

    const SCALE: &str = "nodes.math:Scale";
    const CONSTANT: &str = "nodes.math:Constant";
    const SINK: &str = "nodes.math:Sink";

    fn a_graph_with_a_scale_taking_a_gain_and_a_label() -> Graph {
        a_graph_resolving_these_function_node_types([
            a_function_node_type(SCALE, &["x"], &["result"]).with_config_schema(json!({
                "type": "object",
                "properties": {
                    "gain": {"type": "number", "default": 2.0, "description": "how far it scales"},
                    "label": {"type": "string", "default": "scale"},
                },
            })),
            a_function_node_type(CONSTANT, &[], &["result"]),
            a_function_node_type(SINK, &["value"], &["result"]),
        ])
    }

    fn add_a_node_exposing(
        graph: &mut Graph,
        import_path: &str,
        exposed_config_fields: &[&str],
    ) -> crate::core::Result<crate::core::graph::ProcessorUniqueId> {
        Ok(graph
            .traversal_mut()
            .add_v(
                ProcessorSpec::new(
                    ProcessorClassImportPath::new(import_path).unwrap(),
                    json!({}),
                )
                .with_exposed_config_fields(
                    exposed_config_fields
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                ),
            )?
            .first()
            .unwrap()
            .id
            .clone())
    }

    #[test]
    fn an_exposed_config_field_is_a_channels_input_its_island_carries_linked() {
        let mut graph = a_graph_with_a_scale_taking_a_gain_and_a_label();
        let scale = add_a_node_exposing(&mut graph, SCALE, &["gain"]).unwrap();
        let constant = add_a_node(&mut graph, CONSTANT, json!({}));
        let sink = add_a_node(&mut graph, SINK, json!({}));
        link(&mut graph, (&constant, "result"), (&scale, "gain"));
        let into_the_sink = link(&mut graph, (&scale, "result"), (&sink, "value"));

        let scale_node = graph.traversal().v(&scale).first().unwrap();
        let gain_port = scale_node
            .ports
            .inputs
            .iter()
            .find(|port| port.name == "gain")
            .unwrap();
        assert_eq!(gain_port.family.family, PortFamily::Channels);
        assert_eq!(gain_port.description, "how far it scales");
        assert_eq!(scale_node.exposed_config_fields, vec!["gain".to_string()]);

        let island = the_island_feeding_a_link(&graph, &into_the_sink).unwrap();
        let scale_in_the_island = island["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|island_node| island_node["node"] == scale.to_string())
            .unwrap();
        assert_eq!(
            scale_in_the_island["exposed_config_fields"],
            json!(["gain"])
        );
        assert_eq!(
            scale_in_the_island["links_into"],
            json!([{"port": "gain", "from": {"node": constant.to_string(), "port": "result"}}])
        );
    }

    #[test]
    fn an_unlinked_exposed_config_field_reads_the_config_and_never_waits_for_a_value() {
        let mut graph = a_graph_with_a_scale_taking_a_gain_and_a_label();
        let scale = add_a_node_exposing(&mut graph, SCALE, &["gain"]).unwrap();
        let constant = add_a_node(&mut graph, CONSTANT, json!({}));
        link(&mut graph, (&constant, "result"), (&scale, "x"));

        let scale_node = graph.traversal().v(&scale).first().unwrap();
        assert!(the_inputs_a_function_node_has_no_value_for(&graph, scale_node).is_empty());
    }

    #[test]
    fn a_field_that_is_no_number_or_bool_of_the_config_schema_is_refused_naming_what_it_can_expose()
    {
        let mut graph = a_graph_with_a_scale_taking_a_gain_and_a_label();
        for not_exposable in ["label", "offset"] {
            let refusal = add_a_node_exposing(&mut graph, SCALE, &[not_exposable])
                .unwrap_err()
                .to_string();
            assert!(
                refusal.contains(&format!("cannot expose `{not_exposable}`"))
                    && refusal.contains("It can expose: `gain`"),
                "got {refusal}"
            );
        }
    }

    #[test]
    fn a_field_named_like_a_port_or_named_twice_is_refused() {
        let mut graph = a_graph_resolving_these_function_node_types([a_function_node_type(
            SCALE,
            &["x"],
            &["result"],
        )
        .with_config_schema(json!({
            "type": "object",
            "properties": {"x": {"type": "number"}, "gain": {"type": "number"}},
        }))]);
        let refusal = add_a_node_exposing(&mut graph, SCALE, &["x"])
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("already has a port"), "got {refusal}");
        let refusal = add_a_node_exposing(&mut graph, SCALE, &["gain", "gain"])
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("`gain` as an input twice"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_natively_compiled_node_type_exposes_no_config_field() {
        let mut graph = a_graph_with_a_scale_taking_a_gain_and_a_label();
        let refusal = add_a_node_exposing(
            &mut graph,
            MockSourceTakingOneSetting::processor_class_import_path().as_str(),
            &["frame_width"],
        )
        .unwrap_err()
        .to_string();
        assert!(
            refusal.contains("natively compiled") && refusal.contains("Only a Python node"),
            "got {refusal}"
        );
    }
}
