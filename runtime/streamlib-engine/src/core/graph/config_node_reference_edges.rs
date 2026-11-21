// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Reference edges: a node's config key naming another node of the graph.
//!
//! A reference edge carries no data and is never a link. The step barrier,
//! delivery profiles and link counters never see one; it is read from configs
//! and checked against the graph's nodes and their types.

use streamlib_node_schema::{
    ConfigKeyHoldingANodeReference, config_keys_holding_a_node_reference,
    inputs_holding_a_node_reference, node_name_a_config_value_references,
};

use std::collections::HashMap;

use crate::core::descriptors::NodeClassImportPath;
use crate::core::graph::{
    Graph, NodeRunId, cast_exposed_name_to_url_safe, names_listed_for_a_refusal,
};
use crate::core::json_schema::ConfigNodeReferenceEdgeOnTheWire;
use crate::core::nodes::{
    NodeTypesOneStreamResolves, the_settings_of_a_node_type_of_the_graph_itself,
};
use crate::core::{Error, Result};

/// The keys of `node_type` that hold a node reference: each input of a `NodeReference` it
/// declares, a `SetVariable`'s among them, then each config key its config schema marks that no
/// such input is named; none when the type has neither.
pub(crate) fn config_keys_of_a_node_type_holding_a_node_reference(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
) -> Vec<ConfigKeyHoldingANodeReference> {
    if let Some(settings) = the_settings_of_a_node_type_of_the_graph_itself(node_type) {
        return inputs_holding_a_node_reference(settings);
    }
    let Some(descriptor) = node_types.descriptor(node_type) else {
        return Vec::new();
    };
    let mut keys = inputs_holding_a_node_reference(&descriptor.inputs);
    if let Some(config_schema) = &descriptor.config_schema {
        for marked in config_keys_holding_a_node_reference(config_schema) {
            if !keys.iter().any(|key| key.config_key == marked.config_key) {
                keys.push(marked);
            }
        }
    }
    keys
}

/// One reference edge `node_name`'s config holds, with the type the key it sits
/// under requires.
pub(crate) struct ConfigNodeReferenceEdgeOfOneNode {
    pub(crate) edge: ConfigNodeReferenceEdgeOnTheWire,
    pub(crate) referenced_node_type_import_path: Option<String>,
    pub(crate) one_referrer_per_referenced_node: bool,
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
        one_referrer_per_referenced_node,
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
            edge: ConfigNodeReferenceEdgeOnTheWire {
                node: node_name.to_string(),
                config_key,
                referenced_node: referenced_node.to_string(),
            },
            referenced_node_type_import_path,
            one_referrer_per_referenced_node,
        });
    }
    Ok(edges)
}

/// Refuse a second node naming, under a config key that takes one referrer per
/// referenced node, a node another node already names under that key.
pub(crate) fn refuse_a_second_referrer_of_a_node_that_takes_one<'edge>(
    references: impl IntoIterator<Item = &'edge ConfigNodeReferenceEdgeOfOneNode>,
) -> Result<()> {
    let mut first_referrer_by_key_and_referenced_node: HashMap<(String, String), &str> =
        HashMap::new();
    for reference in references {
        if !reference.one_referrer_per_referenced_node {
            continue;
        }
        let ConfigNodeReferenceEdgeOnTheWire {
            node,
            config_key,
            referenced_node,
        } = &reference.edge;
        let key_and_referenced_node = (
            config_key.clone(),
            cast_exposed_name_to_url_safe(referenced_node)?.into_owned(),
        );
        if let Some(first_referrer) =
            first_referrer_by_key_and_referenced_node.get(&key_and_referenced_node)
        {
            return Err(Error::GraphError(format!(
                "node `{node}`'s config key `{config_key}` names node `{referenced_node}`, which \
                 node `{first_referrer}` already names under `{config_key}`; one node at most \
                 names a node under that key"
            )));
        }
        first_referrer_by_key_and_referenced_node.insert(key_and_referenced_node, node);
    }
    Ok(())
}

/// Refuse `reference` when the node it names is not `referenced_node_type`,
/// the type its config key requires. `referenced_node_type` is the type of the
/// node the graph holds under that name.
pub(crate) fn refuse_a_reference_to_a_node_of_the_wrong_type(
    reference: &ConfigNodeReferenceEdgeOfOneNode,
    referenced_node_type: &NodeClassImportPath,
) -> Result<()> {
    let Some(required_type) = &reference.referenced_node_type_import_path else {
        return Ok(());
    };
    if required_type == referenced_node_type.as_str() {
        return Ok(());
    }
    let ConfigNodeReferenceEdgeOnTheWire {
        node,
        config_key,
        referenced_node,
    } = &reference.edge;
    Err(Error::GraphError(format!(
        "node `{node}`'s config key `{config_key}` names node `{referenced_node}`, which is a \
         `{referenced_node_type}`, and the key takes a `{required_type}`"
    )))
}

pub(crate) fn refuse_a_live_config_naming_a_node_the_graph_does_not_hold(
    graph: &Graph,
    node_types: &NodeTypesOneStreamResolves,
    node_run_id: &NodeRunId,
    config_to_apply: &serde_json::Value,
) -> Result<()> {
    let Some(updated_node) = graph.traversal().v(node_run_id).first() else {
        return Ok(());
    };
    let references = config_node_reference_edges_of_one_node(
        &updated_node.display_name,
        config_to_apply,
        config_keys_of_a_node_type_holding_a_node_reference(node_types, &updated_node.node_type),
    )?;
    if references.is_empty() {
        return Ok(());
    }
    let type_of_each_live_node_by_cast: HashMap<String, NodeClassImportPath> = graph
        .traversal()
        .v(())
        .iter()
        .filter_map(|node| {
            cast_exposed_name_to_url_safe(&node.display_name)
                .ok()
                .map(|cast| (cast.into_owned(), node.node_type.clone()))
        })
        .collect();
    for reference in &references {
        let referenced_node = &reference.edge.referenced_node;
        let Some(referenced_node_type) = type_of_each_live_node_by_cast
            .get(cast_exposed_name_to_url_safe(referenced_node)?.as_ref())
        else {
            return Err(Error::GraphError(format!(
                "input `{}` of node `{}` names node `{referenced_node}`, which the graph \
                 does not hold. The graph holds: {}",
                reference.edge.config_key,
                reference.edge.node,
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
        refuse_a_reference_to_a_node_of_the_wrong_type(reference, referenced_node_type)?;
    }
    if references
        .iter()
        .any(|reference| reference.one_referrer_per_referenced_node)
    {
        let mut references_of_every_other_live_node = Vec::new();
        for live_node in graph.traversal().v(()).iter() {
            if &live_node.id == node_run_id {
                continue;
            }
            let Some(live_config) = &live_node.config else {
                continue;
            };
            references_of_every_other_live_node.extend(config_node_reference_edges_of_one_node(
                &live_node.display_name,
                live_config,
                config_keys_of_a_node_type_holding_a_node_reference(
                    node_types,
                    &live_node.node_type,
                ),
            )?);
        }
        refuse_a_second_referrer_of_a_node_that_takes_one(
            references_of_every_other_live_node
                .iter()
                .chain(references.iter()),
        )?;
    }
    Ok(())
}
