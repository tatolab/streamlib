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
    is_a_shader_node, the_host_input_port_a_member_input_is_read_at,
    the_host_of_the_shader_node_island_of, the_members_of_the_shader_node_island_hosted_by,
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
    /// node's are those of every member of its island, read at the island's host. `set_live`
    /// names each input a client has just set live.
    TheNodeInThisProcess {
        node_instance: Arc<Mutex<NodeInstance>>,
        input_values: Map<String, Value>,
        set_live: Vec<String>,
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
        if let Some(link_wiring) = node.get::<OutOfProcessLinkWiringComponent>() {
            return Self::ItsHelper {
                link_wiring: Arc::clone(&link_wiring.0),
                input_values: the_unlinked_input_values(graph, node),
            };
        }
        let (built_node_id, input_values) = if is_a_shader_node(graph, &node.id) {
            let host_id = the_host_of_the_shader_node_island_of(graph, &node.id);
            let input_values = the_unlinked_input_values_of_the_shader_node_island(graph, &host_id);
            (host_id, input_values)
        } else {
            (node.id.clone(), the_unlinked_input_values(graph, node))
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
                set_live: Vec::new(),
            },
            None => Self::NoNodeBuiltYet,
        }
    }

    /// This delivery, naming `set_live`, the inputs of its node a client has just set live,
    /// when it is to a node in this process; a delivery to a helper, to an island or to a node
    /// not built yet is left as it is, none of them telling a value just set from one held.
    pub(crate) fn naming_the_inputs_set_live(self, set_live: Vec<String>) -> Self {
        match self {
            Self::TheNodeInThisProcess {
                node_instance,
                input_values,
                ..
            } => Self::TheNodeInThisProcess {
                node_instance,
                input_values,
                set_live,
            },
            delivery => delivery,
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
                set_live,
            } => node_instance
                .lock()
                .set_unlinked_input_values_naming_those_set_live(&input_values, &set_live),
            Self::EachIslandEvaluatingIt(_) | Self::NoNodeBuiltYet => Ok(()),
        }
    }

    /// The values a helper or a node in this process is handed; none for an island or a node
    /// not built yet.
    fn input_values(&self) -> Option<&Map<String, Value>> {
        match self {
            Self::ItsHelper { input_values, .. }
            | Self::TheNodeInThisProcess { input_values, .. } => Some(input_values),
            Self::EachIslandEvaluatingIt(_) | Self::NoNodeBuiltYet => None,
        }
    }
}

/// The values each member of the shader node island `host_id` hosts in `graph` reads while no
/// link feeds its inputs, keyed by the input of the host each is read at.
fn the_unlinked_input_values_of_the_shader_node_island(
    graph: &Graph,
    host_id: &NodeRunId,
) -> Map<String, Value> {
    the_members_of_the_shader_node_island_hosted_by(graph, host_id)
        .iter()
        .filter_map(|member_id| graph.traversal().v(member_id).first())
        .flat_map(|member| {
            the_unlinked_input_values(graph, member)
                .into_iter()
                .map(|(input, value)| {
                    let read_at =
                        the_host_input_port_a_member_input_is_read_at(host_id, &member.id, &input);
                    (read_at, value)
                })
        })
        .collect()
}

/// Hand each of `node_ids` in `graph`, once built, the values it reads while no link feeds its
/// inputs, the graph read to find each and released before any is handed; a function node read
/// inside an island is left out, its island carrying its values, and the shader nodes of one
/// island are handed theirs once, through its host.
///
/// The values are read again after they are handed, and a node whose values changed meanwhile is
/// handed them again, so a value set while it was handed its own is the one it is left with. A
/// node not handed its values ends the pass, and the nodes handed theirs before it are read again
/// before the first refusal is answered. After [`HAND_OVER_PASSES_AT_MOST`] passes, a node whose
/// values still change is logged and left; a set landing after the last pass hands that node
/// its own value.
pub(crate) fn deliver_the_unlinked_input_values_of_each_node(
    graph: &RwLock<Graph>,
    node_ids: &[NodeRunId],
) -> Result<()> {
    let mut node_ids_to_hand = the_node_each_is_handed_its_values_through(graph, node_ids);
    let mut first_not_handed_over = None;
    for _ in 0..HAND_OVER_PASSES_AT_MOST {
        if node_ids_to_hand.is_empty() {
            break;
        }
        let deliveries = the_delivery_to_each_node(graph, &node_ids_to_hand);
        let mut handed = Vec::with_capacity(node_ids_to_hand.len());
        for (node_id, delivery) in node_ids_to_hand.into_iter().zip(deliveries) {
            let values_handed = delivery
                .as_ref()
                .and_then(UnlinkedInputValuesDelivery::input_values)
                .cloned();
            if let Some(Err(not_handed_over)) = delivery.map(UnlinkedInputValuesDelivery::hand_over)
            {
                first_not_handed_over.get_or_insert(not_handed_over);
                break;
            }
            handed.push((node_id, values_handed));
        }
        let node_ids_handed: Vec<NodeRunId> =
            handed.iter().map(|(node_id, _)| node_id.clone()).collect();
        let read_again = the_delivery_to_each_node(graph, &node_ids_handed);
        node_ids_to_hand = handed
            .into_iter()
            .zip(read_again)
            .filter(|((_, values_handed), again)| {
                again
                    .as_ref()
                    .and_then(UnlinkedInputValuesDelivery::input_values)
                    != values_handed.as_ref()
            })
            .map(|((node_id, _), _)| node_id)
            .collect();
    }
    if !node_ids_to_hand.is_empty() {
        tracing::warn!(
            nodes = ?node_ids_to_hand,
            "the values of these nodes changed on each of {HAND_OVER_PASSES_AT_MOST} hand-overs; \
             the next set on each hands it its value"
        );
    }
    first_not_handed_over.map_or(Ok(()), Err)
}

/// Each node of `graph` that `node_ids` are handed their values through, once: a shader node
/// through the host of its island, which takes the values of every member, and any other node
/// through itself.
fn the_node_each_is_handed_its_values_through(
    graph: &RwLock<Graph>,
    node_ids: &[NodeRunId],
) -> Vec<NodeRunId> {
    let graph = graph.read();
    let mut handed_through: Vec<NodeRunId> = Vec::with_capacity(node_ids.len());
    for node_id in node_ids {
        let through = if is_a_shader_node(&graph, node_id) {
            the_host_of_the_shader_node_island_of(&graph, node_id)
        } else {
            node_id.clone()
        };
        if !handed_through.contains(&through) {
            handed_through.push(through);
        }
    }
    handed_through
}

/// How many times a compile hands a node its values while they keep changing under it.
const HAND_OVER_PASSES_AT_MOST: usize = 4;

/// How each of `node_ids` in `graph` takes its values, at its index; none for a node the graph
/// no longer holds.
fn the_delivery_to_each_node(
    graph: &RwLock<Graph>,
    node_ids: &[NodeRunId],
) -> Vec<Option<UnlinkedInputValuesDelivery>> {
    let graph = graph.read();
    node_ids
        .iter()
        .map(|node_id| {
            let node = graph.traversal().v(node_id).first()?;
            Some(UnlinkedInputValuesDelivery::of_the_node(&graph, node))
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
