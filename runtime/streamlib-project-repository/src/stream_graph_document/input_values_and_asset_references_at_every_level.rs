// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The input values of each node at every level of a stream document, and the asset references
//! they hold.

use serde_json::{Map, Value};
use uuid::Uuid;

use super::node_uuid_of;
use crate::stream_document_upgrade::NODE_INPUT_VALUES_KEY;

/// Make each reference in `value` to an asset of the project it was written in,
/// `{"asset": "<uuid>"}`, a reference into that project as an import, adding
/// `"from": "<project>"`; a reference already naming a project is left as it is.
pub fn asset_references_made_references_into_the_import(value: &mut Value, project: Uuid) {
    match value {
        Value::Object(object) => {
            let names_an_asset = object
                .get("asset")
                .and_then(Value::as_str)
                .is_some_and(|asset| Uuid::parse_str(asset).is_ok());
            if names_an_asset {
                object
                    .entry("from")
                    .or_insert_with(|| Value::String(project.to_string()));
            }
            for entry in object.values_mut() {
                asset_references_made_references_into_the_import(entry, project);
            }
        }
        Value::Array(items) => {
            for item in items {
                asset_references_made_references_into_the_import(item, project);
            }
        }
        _ => {}
    }
}

pub(super) fn push_the_input_values_of_each_node_at_every_level<'fragment>(
    fragment: &'fragment Value,
    input_values: &mut Vec<(Uuid, &'fragment Map<String, Value>)>,
) {
    if let Some(nodes) = fragment.get("nodes").and_then(Value::as_array) {
        push_the_input_values_of_each_of_the_nodes_at_every_level(nodes, input_values);
    }
}

pub(super) fn push_the_input_values_of_each_of_the_nodes_at_every_level<'fragment>(
    nodes: &'fragment [Value],
    input_values: &mut Vec<(Uuid, &'fragment Map<String, Value>)>,
) {
    for node in nodes.iter().filter_map(Value::as_object) {
        if let (Some(node_uuid), Some(Value::Object(values))) =
            (node_uuid_of(node), node.get(NODE_INPUT_VALUES_KEY))
        {
            input_values.push((node_uuid, values));
        }
        if let Some(inner_fragment) = node.get("fragment") {
            push_the_input_values_of_each_node_at_every_level(inner_fragment, input_values);
        }
    }
}

/// Every asset of this project a node's input values reference, `{"asset": "<uuid>"}` at any
/// depth, by UUID; a reference naming another project with `from` is that project's to check.
pub fn assets_a_nodes_input_values_reference(input_values: &Map<String, Value>) -> Vec<Uuid> {
    let mut referenced = Vec::new();
    for value in input_values.values() {
        collect_asset_references(value, &mut referenced);
    }
    referenced
}

fn collect_asset_references(value: &Value, referenced: &mut Vec<Uuid>) {
    match value {
        Value::Object(object) => {
            if !object.contains_key("from")
                && let Some(asset) = object
                    .get("asset")
                    .and_then(Value::as_str)
                    .and_then(|asset| Uuid::parse_str(asset).ok())
            {
                referenced.push(asset);
            }
            for entry in object.values() {
                collect_asset_references(entry, referenced);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_asset_references(item, referenced);
            }
        }
        _ => {}
    }
}
