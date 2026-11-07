// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reference edges: a node's config key naming another node of the graph.
//!
//! A reference edge carries no data and is never a link. The step barrier,
//! delivery profiles and link counters never see one; it is read from configs
//! and checked against the graph's nodes and their types.

use streamlib_processor_schema::{
    ConfigKeyHoldingANodeReference, config_keys_holding_a_node_reference,
    node_name_a_config_value_references,
};

use std::collections::HashMap;

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{
    Graph, ProcessorUniqueId, cast_exposed_name_to_url_safe, names_listed_for_a_refusal,
};
use crate::core::json_schema::ConfigNodeReferenceEdgeOutput;
use crate::core::processors::NodeTypesOneStreamResolves;
use crate::core::{Error, Result};

/// The config keys of `node_type` that hold a node reference, from the config
/// schema its descriptor carries; none when the type has no descriptor or no
/// schema.
pub(crate) fn config_keys_of_a_node_type_holding_a_node_reference(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &ProcessorClassImportPath,
) -> Vec<ConfigKeyHoldingANodeReference> {
    node_types
        .descriptor(node_type)
        .and_then(|descriptor| descriptor.config_schema)
        .map(|config_schema| config_keys_holding_a_node_reference(&config_schema))
        .unwrap_or_default()
}

/// One reference edge `node_name`'s config holds, with the type the key it sits
/// under requires.
pub(crate) struct ConfigNodeReferenceEdgeOfOneNode {
    pub(crate) edge: ConfigNodeReferenceEdgeOutput,
    pub(crate) referenced_node_type_import_path: Option<String>,
}

/// The reference edges `config` holds under `keys_holding_a_node_reference`. A
/// key left out or `null` holds none, as does another value a key also takes;
/// any other value than `{"node": "<name>"}` is refused naming the key.
pub(crate) fn config_node_reference_edges_of_one_node(
    node_name: &str,
    config: &serde_json::Value,
    keys_holding_a_node_reference: Vec<ConfigKeyHoldingANodeReference>,
) -> Result<Vec<ConfigNodeReferenceEdgeOfOneNode>> {
    let mut edges = Vec::new();
    for ConfigKeyHoldingANodeReference {
        config_key,
        referenced_node_type_import_path,
        takes_a_value_other_than_a_node_reference,
    } in keys_holding_a_node_reference
    {
        let Some(config_value) = config.get(&config_key).filter(|value| !value.is_null()) else {
            continue;
        };
        let Some(referenced_node) = node_name_a_config_value_references(config_value) else {
            if takes_a_value_other_than_a_node_reference {
                continue;
            }
            return Err(Error::GraphError(format!(
                "node `{node_name}`'s config key `{config_key}` holds {config_value}, and the \
                 key holds a reference to another node of the graph, written \
                 {{\"node\": \"<name>\"}}. Pass the reference `add` returned for that node"
            )));
        };
        edges.push(ConfigNodeReferenceEdgeOfOneNode {
            edge: ConfigNodeReferenceEdgeOutput {
                node: node_name.to_string(),
                config_key,
                referenced_node: referenced_node.to_string(),
            },
            referenced_node_type_import_path,
        });
    }
    Ok(edges)
}

/// Refuse `reference` when the node it names is not `referenced_node_type`,
/// the type its config key requires. `referenced_node_type` is the type of the
/// node the graph holds under that name.
pub(crate) fn refuse_a_reference_to_a_node_of_the_wrong_type(
    reference: &ConfigNodeReferenceEdgeOfOneNode,
    referenced_node_type: &ProcessorClassImportPath,
) -> Result<()> {
    let Some(required_type) = &reference.referenced_node_type_import_path else {
        return Ok(());
    };
    if required_type == referenced_node_type.as_str() {
        return Ok(());
    }
    let ConfigNodeReferenceEdgeOutput {
        node,
        config_key,
        referenced_node,
    } = &reference.edge;
    Err(Error::GraphError(format!(
        "node `{node}`'s config key `{config_key}` names node `{referenced_node}`, which is a \
         `{referenced_node_type}`, and the key takes a `{required_type}`"
    )))
}

/// Refuse a live config update of `processor_id` whose node references name a
/// node the live graph does not hold, or one of the wrong type.
pub(crate) fn refuse_a_config_update_naming_a_node_the_graph_does_not_hold(
    graph: &Graph,
    node_types: &NodeTypesOneStreamResolves,
    processor_id: &ProcessorUniqueId,
    config_to_apply: &serde_json::Value,
) -> Result<()> {
    let Some(updated_node) = graph.traversal().v(processor_id).first() else {
        return Ok(());
    };
    let references = config_node_reference_edges_of_one_node(
        &updated_node.display_name,
        config_to_apply,
        config_keys_of_a_node_type_holding_a_node_reference(
            node_types,
            &updated_node.processor_type,
        ),
    )?;
    if references.is_empty() {
        return Ok(());
    }
    let type_of_each_live_node_by_cast: HashMap<String, ProcessorClassImportPath> = graph
        .traversal()
        .v(())
        .iter()
        .filter_map(|node| {
            cast_exposed_name_to_url_safe(&node.display_name)
                .ok()
                .map(|cast| (cast.into_owned(), node.processor_type.clone()))
        })
        .collect();
    for reference in references {
        let referenced_node = &reference.edge.referenced_node;
        let Some(referenced_node_type) = type_of_each_live_node_by_cast
            .get(cast_exposed_name_to_url_safe(referenced_node)?.as_ref())
        else {
            return Err(Error::GraphError(format!(
                "node `{}`'s config key `{}` names node `{referenced_node}`, which the graph \
                 does not hold. The graph holds: {}",
                reference.edge.node,
                reference.edge.config_key,
                names_listed_for_a_refusal(
                    {
                        let mut names: Vec<&String> =
                            type_of_each_live_node_by_cast.keys().collect();
                        names.sort();
                        names
                    },
                    "no node"
                )
            )));
        };
        refuse_a_reference_to_a_node_of_the_wrong_type(&reference, referenced_node_type)?;
    }
    Ok(())
}
