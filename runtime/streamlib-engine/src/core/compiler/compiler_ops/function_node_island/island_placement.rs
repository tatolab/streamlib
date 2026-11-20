// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Placing an island's function nodes, each after what it reads, into the description its reader's setup command carries.

use std::collections::HashSet;

use serde_json::Value as JsonValue;

use crate::core::NodeRunId;
use crate::core::error::{Error, Result};
use crate::core::graph::{Graph, Link, LinkUniqueId};

use super::{
    is_a_previous_value_node, link_in, runs_inside_the_read_of_each_node_it_feeds,
    the_reader_input_port_a_link_into_a_function_node_is_read_at,
};

/// The island upstream of `link_id` as its reader's setup command carries it:
/// the reader's input port, the island output feeding it, and every function
/// node it calls, each after the nodes it reads.
///
/// Wire contract with the helper bootstrap: `{"input_port", "link_id",
/// "output": {"node", "port"}, "nodes": [{"node", "name", "import_path",
/// "config", "pins": {input: value}, "links_into": [{"port", "from": {"node",
/// "port"} or {"reader_input_port"}}]}]}`, where `"reads_members_over_its_pin": true` beside
/// `"reader_input_port"` marks a bundle input fed member by member, whose reader port carries the
/// members its links have carried, each newest, to put over the input's pin; a stateful node's
/// `"node_uuid"` and `"state_fields"` beside its `"node"`, and the `"exposed_config_fields"` of a
/// node that takes config fields as inputs, each a port a link of `"links_into"` may name.
pub(crate) fn the_island_feeding_a_link(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<JsonValue> {
    let link = link_in(graph, link_id)?;
    let island_output_node_id = link.source().node_run_id().clone();
    let mut nodes_each_after_what_it_reads = Vec::new();
    let mut placed = HashSet::new();
    let mut read_at_the_last_evaluation_by_a_previous_value_node =
        vec![island_output_node_id.clone()];
    while let Some(node_id) = read_at_the_last_evaluation_by_a_previous_value_node.pop() {
        place_a_function_node_after_what_it_reads(
            graph,
            link_id,
            &node_id,
            &mut placed,
            &mut Vec::new(),
            &mut nodes_each_after_what_it_reads,
            &mut read_at_the_last_evaluation_by_a_previous_value_node,
        )?;
    }
    Ok(serde_json::json!({
        "input_port": link.to_port().port_name(),
        "link_id": link_id.to_string(),
        "output": {
            "node": island_output_node_id.to_string(),
            "port": link.source().port_name(),
        },
        "nodes": nodes_each_after_what_it_reads,
    }))
}

fn place_a_function_node_after_what_it_reads(
    graph: &Graph,
    island_link_id: &LinkUniqueId,
    node_id: &NodeRunId,
    placed: &mut HashSet<NodeRunId>,
    on_the_path_from_the_output: &mut Vec<NodeRunId>,
    nodes_each_after_what_it_reads: &mut Vec<JsonValue>,
    read_at_the_last_evaluation_by_a_previous_value_node: &mut Vec<NodeRunId>,
) -> Result<()> {
    if placed.contains(node_id) {
        return Ok(());
    }
    if let Some(cycle_start) = on_the_path_from_the_output
        .iter()
        .position(|on_the_path| on_the_path == node_id)
    {
        let cycle: Vec<String> = on_the_path_from_the_output[cycle_start..]
            .iter()
            .chain(std::iter::once(node_id))
            .map(ToString::to_string)
            .collect();
        return Err(Error::InvalidGraph(format!(
            "function nodes {} read each other in a cycle; an island evaluates each node after \
             what it reads, so a cycle closes only through a `PreviousValue` node, which hands out \
             its input as it was at the last evaluation",
            cycle.join(" → ")
        )));
    }
    let node = graph
        .traversal()
        .v(node_id)
        .first()
        .ok_or_else(|| Error::NodeNotFound(node_id.to_string()))?;
    let import_path = node.node_type.to_string();
    let node_name = node.display_name.clone();
    let config = node.config.clone().unwrap_or(JsonValue::Null);
    let pins = JsonValue::Object(
        crate::core::graph::input_values::the_pin_values_with_the_live_ones_over_them(graph, node),
    );

    on_the_path_from_the_output.push(node_id.clone());
    let links_into_this_node: Vec<&Link> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| link.to_port().node_run_id() == node_id)
        .collect();
    let mut links_into: Vec<JsonValue> = Vec::with_capacity(links_into_this_node.len());
    for link_into in links_into_this_node {
        let upstream_id = link_into.source().node_run_id();
        if !runs_inside_the_read_of_each_node_it_feeds(graph, upstream_id) {
            let port = link_into.to_port().port_name();
            let reader_input_port = the_reader_input_port_a_link_into_a_function_node_is_read_at(
                link_into,
                island_link_id,
            );
            let reads_members_over_its_pin = link_into.to_port().member().is_some();
            let read_at_that_port_already = links_into.iter().any(|placed_link| {
                placed_link["from"]["reader_input_port"].as_str()
                    == Some(reader_input_port.as_str())
            });
            if !read_at_that_port_already {
                let mut from = serde_json::json!({ "reader_input_port": reader_input_port });
                if reads_members_over_its_pin {
                    from["reads_members_over_its_pin"] = JsonValue::Bool(true);
                }
                links_into.push(serde_json::json!({ "port": port, "from": from }));
            }
            continue;
        }
        if is_a_previous_value_node(graph, node_id) {
            read_at_the_last_evaluation_by_a_previous_value_node.push(upstream_id.clone());
        } else {
            place_a_function_node_after_what_it_reads(
                graph,
                island_link_id,
                upstream_id,
                placed,
                on_the_path_from_the_output,
                nodes_each_after_what_it_reads,
                read_at_the_last_evaluation_by_a_previous_value_node,
            )?;
        }
        links_into.push(serde_json::json!({
            "port": link_into.to_port().port_name(),
            "from": {
                "node": upstream_id.to_string(),
                "port": link_into.source().port_name(),
            },
        }));
    }
    on_the_path_from_the_output.pop();

    placed.insert(node_id.clone());
    let mut island_node = serde_json::json!({
        "node": node_id.to_string(),
        "name": node_name,
        "import_path": import_path,
        "config": config,
        "pins": pins,
        "links_into": links_into,
    });
    let state_fields = the_state_fields_of_a_function_node(graph, &node.node_type);
    if !state_fields.is_empty() {
        island_node["node_uuid"] = serde_json::json!(node.node_uuid);
        island_node["state_fields"] = serde_json::json!(state_fields);
    }
    if !node.exposed_config_fields.is_empty() {
        island_node["exposed_config_fields"] = serde_json::json!(node.exposed_config_fields);
    }
    nodes_each_after_what_it_reads.push(island_node);
    Ok(())
}

/// The state record fields a function node of `node_type` carries; none for a
/// stateless one.
pub(crate) fn the_state_fields_of_a_function_node(
    graph: &Graph,
    node_type: &crate::core::descriptors::NodeClassImportPath,
) -> Vec<String> {
    graph
        .node_types_this_stream_resolves()
        .descriptor(node_type)
        .map(|descriptor| descriptor.function_node_state_fields)
        .unwrap_or_default()
}
