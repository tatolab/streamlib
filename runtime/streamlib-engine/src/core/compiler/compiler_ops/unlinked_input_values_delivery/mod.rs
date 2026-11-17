// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values one node reads while no link feeds its inputs, handed to that node alone, however
//! it is hosted: its helper through `set_input_values`, the node in this process through its
//! own `set_unlinked_input_values`, and a function node read inside an island by handing that
//! island again. The graph is read only to find the node and what it reads; the node is handed
//! its values with the graph's lock released.

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value};

use super::function_node_island::runs_inside_the_read_of_each_node_it_feeds;
use super::shader_node_island::{
    is_a_shader_node, the_host_input_port_a_fused_shader_node_input_is_read_at,
    the_host_of_the_shader_node_island_of,
};
use crate::core::error::Result;
use crate::core::graph::input_values::the_unlinked_input_values;
use crate::core::graph::{
    Graph, GraphNode, GraphNodeWithComponents, NodeInstanceComponent, NodeRunId,
    OutOfProcessLinkWiringComponent,
};
use crate::core::nodes::{NodeInstance, OutOfProcessLinkWiringEnvelope};

/// How the values one node reads while no link feeds its inputs reach it.
pub(crate) enum UnlinkedInputValuesDelivery {
    /// Through the helper the node runs in, which takes them by the unanswered
    /// `set_input_values`.
    ItsHelper {
        link_wiring: Arc<OutOfProcessLinkWiringEnvelope>,
        input_values: Map<String, Value>,
    },
    /// Through the node built in this process, keyed by the input each is read at: a shader
    /// node fused into another's island is read at that island's host.
    TheNodeInThisProcess {
        node_instance: Arc<Mutex<NodeInstance>>,
        input_values: Map<String, Value>,
    },
    /// Through each island evaluating the function node, handed again; its caller hands them
    /// under the graph's write lock.
    EachIslandEvaluatingIt(NodeRunId),
    /// Nowhere yet: nothing is built for the node, so it takes them when it is built.
    NoNodeBuiltYet,
}

impl UnlinkedInputValuesDelivery {
    /// How the values `node` of `graph` reads while no link feeds its inputs reach it, with
    /// those values.
    pub(crate) fn of_the_node(graph: &Graph, node: &GraphNode) -> Self {
        if runs_inside_the_read_of_each_node_it_feeds(graph, &node.id) {
            return Self::EachIslandEvaluatingIt(node.id.clone());
        }
        let input_values = the_unlinked_input_values(graph, node);
        if let Some(link_wiring) = node.get::<OutOfProcessLinkWiringComponent>() {
            return Self::ItsHelper {
                link_wiring: Arc::clone(&link_wiring.0),
                input_values,
            };
        }
        let (built_node_id, input_values) = if is_a_shader_node(graph, &node.id) {
            let host_id = the_host_of_the_shader_node_island_of(graph, &node.id);
            let input_values = if host_id == node.id {
                input_values
            } else {
                input_values
                    .into_iter()
                    .map(|(input, value)| {
                        let read_at = the_host_input_port_a_fused_shader_node_input_is_read_at(
                            &host_id, &node.id, &input,
                        );
                        (read_at.port_name().to_string(), value)
                    })
                    .collect()
            };
            (host_id, input_values)
        } else {
            (node.id.clone(), input_values)
        };
        match graph
            .traversal()
            .v(&built_node_id)
            .first()
            .and_then(|built| built.get::<NodeInstanceComponent>())
        {
            Some(node_instance) => Self::TheNodeInThisProcess {
                node_instance: Arc::clone(&node_instance.0),
                input_values,
            },
            None => Self::NoNodeBuiltYet,
        }
    }

    /// Hand a helper or a node in this process its values; an island, which is handed under
    /// the graph's write lock, and a node not built yet are left to their callers.
    pub(crate) fn hand_over(self) -> Result<()> {
        match self {
            Self::ItsHelper {
                link_wiring,
                input_values,
            } => link_wiring
                .record_the_unlinked_input_values_and_hand_them_to_a_far_side_past_its_setup_command(
                    input_values,
                ),
            Self::TheNodeInThisProcess {
                node_instance,
                input_values,
            } => node_instance.lock().set_unlinked_input_values(&input_values),
            Self::EachIslandEvaluatingIt(_) | Self::NoNodeBuiltYet => Ok(()),
        }
    }
}

/// Hand each of `node_ids` in `graph`, once built, the values it reads while no link feeds its
/// inputs, the graph read to find each and released before any is handed; a function node read
/// inside an island is left out, its island carrying its values.
pub(crate) fn deliver_the_unlinked_input_values_of_each_node(
    graph: &RwLock<Graph>,
    node_ids: &[NodeRunId],
) -> Result<()> {
    let deliveries: Vec<UnlinkedInputValuesDelivery> = {
        let graph = graph.read();
        node_ids
            .iter()
            .filter_map(|node_id| graph.traversal().v(node_id).first())
            .map(|node| UnlinkedInputValuesDelivery::of_the_node(&graph, node))
            .collect()
    };
    deliveries
        .into_iter()
        .try_for_each(UnlinkedInputValuesDelivery::hand_over)
}

#[cfg(test)]
pub(crate) mod tests;
