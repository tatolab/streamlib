// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Moving a function node out of its readers' islands into a helper process of
//! its own: each stateful one, alone, as the compile adds it; and on a crash or
//! stall of its reader's helper, with every function node downstream of it, so
//! the running reader's islands lose nodes and gain no input port.

use std::collections::{BTreeSet, HashSet};

use super::function_node_island::{
    is_a_stateful_function_node, runs_inside_the_read_of_each_node_it_feeds,
    the_report_of_how_its_helpers_ran_a_function_node,
};
use super::open_iceoryx2_service_op::close_iceoryx2_service;
use crate::core::NodeRunId;
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, FunctionNodePromotionOutOfItsReadersIslands,
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkState, LinkStateComponent,
    LinkUniqueId, NodeMetrics, OutOfProcessLinkWiringComponent,
};

/// How often a promoted function node's helper runs it when the reader it
/// crashed has no frame interval of its own.
pub(crate) const PROMOTED_FUNCTION_NODE_INTERVAL_MS_FOR_A_READER_WITH_NONE: u32 = 10;

/// How often the helper of a stateful function node evaluates it.
pub(crate) const STATEFUL_FUNCTION_NODE_INTERVAL_MS: u32 = 10;

/// Place each stateful function node in `nodes_to_add` in a helper process of
/// its own, so its state has one copy whatever reads it, and a reader wired to
/// it while the stream plays is one more link out of that helper.
///
/// The function nodes downstream of it stay in their readers' islands, each
/// reading its value at an input port of that reader.
pub(crate) fn place_each_stateful_function_node_in_a_process_of_its_own(
    graph: &mut Graph,
    nodes_to_add: &[NodeRunId],
) {
    let stateful_function_nodes_added: Vec<NodeRunId> = nodes_to_add
        .iter()
        .filter(|node_id| {
            graph.traversal().v(*node_id).first().is_some_and(|node| {
                is_a_stateful_function_node(graph, node)
                    && !node.has::<FunctionNodePromotedToItsOwnProcessComponent>()
            })
        })
        .cloned()
        .collect();
    for stateful_function_node in stateful_function_nodes_added {
        mark_a_function_node_promoted_to_its_own_process(
            graph,
            &stateful_function_node,
            ProcessExecution::Continuous {
                interval_ms: STATEFUL_FUNCTION_NODE_INTERVAL_MS,
            },
        );
    }
}

/// Mark `function_node_id` promoted into a helper of its own driven the way
/// `process_execution` says, sharing the report `graph` renders as its
/// `ran_compiled` with that helper, and evaluated as it sets up when an output
/// of it feeds a setting.
fn mark_a_function_node_promoted_to_its_own_process(
    graph: &mut Graph,
    function_node_id: &NodeRunId,
    process_execution: ProcessExecution,
) {
    let Some(how_its_own_helper_ran_it) =
        the_report_of_how_its_helpers_ran_a_function_node(graph, function_node_id)
    else {
        return;
    };
    let evaluated_at_setup = graph
        .traversal()
        .v(function_node_id)
        .out_e()
        .iter()
        .any(|link| {
            graph.is_a_setting_whose_wire_the_engine_reads(
                link.to_port().node_run_id(),
                link.to_port().port_name(),
            )
        });
    if let Some(node) = graph.traversal_mut().v(function_node_id).first_mut() {
        node.insert_component_without_rendering_it(FunctionNodePromotedToItsOwnProcessComponent {
            process_execution,
            how_its_own_helper_ran_it,
            evaluated_at_setup,
        });
    }
}

/// What a promotion adds to the compile that carries it.
#[derive(Debug, Default)]
pub(crate) struct FunctionNodePromotionPlanned {
    pub(crate) nodes_to_add: Vec<NodeRunId>,
    /// Each link into or out of a promoted node, wired again by what its two
    /// ends now are.
    pub(crate) links_to_add: Vec<LinkUniqueId>,
    /// What `graph` renders under each promoted node's `metrics` once wired.
    pub(crate) promotion_of_each_node:
        Vec<(NodeRunId, FunctionNodePromotionOutOfItsReadersIslands)>,
}

/// What the compile that carries a promotion removes from the graph.
pub(crate) struct WhatTheCompileCarryingAPromotionRemoves<'a> {
    pub(crate) nodes: &'a [NodeRunId],
    pub(crate) links: &'a [LinkUniqueId],
}

pub(crate) fn plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
    graph: &mut Graph,
    crashed_function_node: &NodeRunId,
    reader: &NodeRunId,
    reason: &str,
    what_the_same_compile_removes: &WhatTheCompileCarryingAPromotionRemoves<'_>,
) -> Result<FunctionNodePromotionPlanned> {
    let crashed_function_node_name = the_name_in_the_graph_of(graph, crashed_function_node);
    let reader_name = the_name_in_the_graph_of(graph, reader);
    if !runs_inside_the_read_of_each_node_it_feeds(graph, crashed_function_node) {
        return Err(Error::InvalidGraph(format!(
            "`{crashed_function_node_name}` is not a function node inside a reader's island, so \
             it is not promoted"
        )));
    }
    let promoted = the_function_nodes_from(graph, crashed_function_node);
    let other_readers: BTreeSet<String> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| promoted.contains(link.source().node_run_id()))
        .map(|link| link.to_port().node_run_id())
        .filter(|destination| !promoted.contains(*destination) && *destination != reader)
        .map(|other_reader| the_name_in_the_graph_of(graph, other_reader))
        .collect();
    if !other_readers.is_empty() {
        return Err(Error::InvalidGraph(format!(
            "function node `{crashed_function_node_name}` is not promoted out of \
             `{reader_name}`'s islands: the nodes it would move also feed {}, and a running \
             reader is not re-islanded in this release",
            other_readers
                .iter()
                .map(|other_reader| format!("`{other_reader}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let reader_link_wiring = graph
        .traversal()
        .v(reader)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .map(|component| std::sync::Arc::clone(&component.0))
        .ok_or_else(|| {
            Error::InvalidGraph(format!(
                "`{reader_name}` runs no helper process, so no function node is promoted out of \
                 it"
            ))
        })?;
    let process_execution = match reader_link_wiring.far_side_process_execution() {
        continuous @ ProcessExecution::Continuous { .. } => continuous,
        _ => ProcessExecution::Continuous {
            interval_ms: PROMOTED_FUNCTION_NODE_INTERVAL_MS_FOR_A_READER_WITH_NONE,
        },
    };

    let links_touching_a_promoted_node: Vec<(LinkUniqueId, bool)> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            promoted.contains(link.source().node_run_id())
                || promoted.contains(link.to_port().node_run_id())
        })
        .map(|link| (link.id.clone(), link.to_port().node_run_id() == reader))
        .collect();
    let removed_by_the_same_compile = promoted
        .iter()
        .chain(std::iter::once(reader))
        .any(|node_id| what_the_same_compile_removes.nodes.contains(node_id))
        || links_touching_a_promoted_node
            .iter()
            .any(|(link_id, _)| what_the_same_compile_removes.links.contains(link_id));
    if removed_by_the_same_compile {
        return Err(Error::InvalidGraph(format!(
            "function node `{crashed_function_node_name}` is not promoted out of \
             `{reader_name}`'s islands: the same change removes the reader, a node that would \
             move, or one of their links"
        )));
    }
    for (link_id, feeds_the_reader) in &links_touching_a_promoted_node {
        if *feeds_the_reader {
            reader_link_wiring.forget_a_function_node_island(link_id.as_str());
        }
        let read_through_its_islands_reader =
            graph.traversal().e(link_id).first().is_some_and(|link| {
                link.has::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
            });
        if read_through_its_islands_reader {
            close_iceoryx2_service(graph, link_id)?;
        }
        if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
            link.insert(LinkStateComponent(LinkState::Pending));
        }
    }

    let mut planned = FunctionNodePromotionPlanned::default();
    for promoted_id in the_promoted_nodes_in_a_stable_order(&promoted) {
        mark_a_function_node_promoted_to_its_own_process(graph, &promoted_id, process_execution);
        planned.promotion_of_each_node.push((
            promoted_id.clone(),
            FunctionNodePromotionOutOfItsReadersIslands {
                after_crashing_the_helper_of: reader_name.clone(),
                reason: reason.to_string(),
            },
        ));
        planned.nodes_to_add.push(promoted_id);
    }
    planned.links_to_add = links_touching_a_promoted_node
        .into_iter()
        .map(|(link_id, _)| link_id)
        .collect();
    Ok(planned)
}

/// Render each promotion `planned` made under its node's `metrics`.
pub(crate) fn record_each_promotion_on_its_nodes_metrics(
    graph: &mut Graph,
    planned: &FunctionNodePromotionPlanned,
) {
    for (promoted_id, promotion) in &planned.promotion_of_each_node {
        let Some(node) = graph.traversal_mut().v(promoted_id).first_mut() else {
            continue;
        };
        if let Some(metrics) = node.get_mut::<NodeMetrics>() {
            metrics.promotion_out_of_its_readers_islands = Some(promotion.clone());
        } else {
            node.insert(NodeMetrics {
                promotion_out_of_its_readers_islands: Some(promotion.clone()),
                ..Default::default()
            });
        }
    }
}

/// The name `node_id` carries in the graph, or its id when no node has it.
fn the_name_in_the_graph_of(graph: &Graph, node_id: &NodeRunId) -> String {
    graph
        .traversal()
        .v(node_id)
        .first()
        .map(|node| node.display_name.clone())
        .unwrap_or_else(|| node_id.to_string())
}

/// `function_node` and every function node its outputs reach inside islands.
fn the_function_nodes_from(graph: &Graph, function_node: &NodeRunId) -> HashSet<NodeRunId> {
    let mut reached = HashSet::new();
    let mut to_follow = vec![function_node.clone()];
    while let Some(following) = to_follow.pop() {
        if !reached.insert(following.clone()) {
            continue;
        }
        for link in graph.traversal().e(()).iter() {
            let destination = link.to_port().node_run_id();
            if link.source().node_run_id() == &following
                && runs_inside_the_read_of_each_node_it_feeds(graph, destination)
            {
                to_follow.push(destination.clone());
            }
        }
    }
    reached
}

fn the_promoted_nodes_in_a_stable_order(promoted: &HashSet<NodeRunId>) -> Vec<NodeRunId> {
    let mut in_order: Vec<NodeRunId> = promoted.iter().cloned().collect();
    in_order.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    in_order
}

#[cfg(test)]
mod tests;
