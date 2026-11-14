// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Moving a function node out of its readers' islands into a helper process of
//! its own, with every function node downstream of it, so no node left inline
//! reads one that moved: on a crash or stall of its reader's helper, and when
//! the compile finds a stateful one that more than one reader would evaluate.

use std::collections::{BTreeSet, HashSet};

use super::function_node_island::{
    runs_inside_the_read_of_each_node_it_feeds, the_report_of_how_its_helpers_ran_a_function_node,
    the_state_fields_of_a_function_node,
};
use super::open_iceoryx2_service_op::close_iceoryx2_service;
use crate::core::ProcessorUniqueId;
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, FunctionNodePromotionOutOfItsReadersIslands,
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkState, LinkStateComponent,
    LinkUniqueId, OutOfProcessLinkWiringComponent, ProcessorMetrics,
};

/// How often a promoted function node's helper runs it when the reader it
/// crashed has no frame interval of its own.
pub(crate) const PROMOTED_FUNCTION_NODE_INTERVAL_MS_FOR_A_READER_WITH_NONE: u32 = 10;

/// How often the helper of a stateful function node that several readers read
/// evaluates it.
pub(crate) const STATEFUL_FUNCTION_NODE_SEVERAL_READERS_READ_INTERVAL_MS: u32 = 10;

/// Move each stateful function node that more than one reader would evaluate,
/// with every function node downstream of it, out of the islands into a helper
/// process of its own, so its state has one copy and every reader reads the one
/// value it evaluates.
///
/// Refused by name, changing nothing, when one of those readers or the node
/// itself is already running, since a running reader is not re-islanded in
/// this release.
pub(crate) fn place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
    graph: &mut Graph,
    processors_to_add: &[ProcessorUniqueId],
    processors_to_remove: &[ProcessorUniqueId],
) -> Result<()> {
    let mut moved_out_of_the_islands: HashSet<ProcessorUniqueId> = HashSet::new();
    loop {
        let runs_inline = |node_id: &ProcessorUniqueId| {
            runs_inside_the_read_of_each_node_it_feeds(graph, node_id)
                && !moved_out_of_the_islands.contains(node_id)
                && !processors_to_remove.contains(node_id)
        };
        let mut stateful_function_nodes_inline: Vec<ProcessorUniqueId> = graph
            .traversal()
            .v(())
            .ids()
            .into_iter()
            .filter(|node_id| runs_inline(node_id))
            .filter(|node_id| {
                graph.traversal().v(node_id).first().is_some_and(|node| {
                    !the_state_fields_of_a_function_node(graph, &node.processor_type).is_empty()
                })
            })
            .collect();
        stateful_function_nodes_inline.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let read_by_several_readers =
            stateful_function_nodes_inline
                .into_iter()
                .find_map(|node_id| {
                    let readers =
                        the_readers_evaluating(graph, &node_id, &runs_inline, processors_to_remove);
                    (readers.len() > 1).then_some((node_id, readers))
                });
        let Some((stateful_function_node, readers)) = read_by_several_readers else {
            break;
        };
        let stateful_function_node_name = the_name_in_the_graph_of(graph, &stateful_function_node);
        let running: BTreeSet<String> = readers
            .iter()
            .chain(std::iter::once(&stateful_function_node))
            .filter(|node_id| !processors_to_add.contains(*node_id))
            .map(|node_id| the_name_in_the_graph_of(graph, node_id))
            .collect();
        if !running.is_empty() {
            return Err(Error::InvalidGraph(format!(
                "stateful function node `{stateful_function_node_name}` would be read by {} \
                 readers, so it runs once in a helper process of its own, and {} already \
                 running; a running reader is not re-islanded in this release. To fix: stop \
                 the stream and start it with the edit",
                readers.len(),
                running
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
                    + if running.len() == 1 { " is" } else { " are" }
            )));
        }
        let moved = the_function_nodes_downstream_of(graph, &stateful_function_node, &runs_inline);
        if let Some(not_added) = moved
            .iter()
            .find(|node_id| !processors_to_add.contains(*node_id))
        {
            return Err(Error::InvalidGraph(format!(
                "stateful function node `{stateful_function_node_name}` would be read by {} \
                 readers, so it runs once in a helper process of its own with every function \
                 node downstream of it, and `{}` is already running inside a reader; a \
                 running reader is not re-islanded in this release. To fix: stop the stream \
                 and start it with the edit",
                readers.len(),
                the_name_in_the_graph_of(graph, not_added)
            )));
        }
        moved_out_of_the_islands.extend(moved);
    }
    for moved_id in the_promoted_nodes_in_a_stable_order(&moved_out_of_the_islands) {
        mark_a_function_node_promoted_to_its_own_process(
            graph,
            &moved_id,
            ProcessExecution::Continuous {
                interval_ms: STATEFUL_FUNCTION_NODE_SEVERAL_READERS_READ_INTERVAL_MS,
            },
        );
    }
    Ok(())
}

/// Mark `function_node_id` promoted into a helper of its own driven the way
/// `process_execution` says, sharing the report `graph` renders as its
/// `ran_compiled` with that helper.
fn mark_a_function_node_promoted_to_its_own_process(
    graph: &mut Graph,
    function_node_id: &ProcessorUniqueId,
    process_execution: ProcessExecution,
) {
    let Some(how_its_own_helper_ran_it) =
        the_report_of_how_its_helpers_ran_a_function_node(graph, function_node_id)
    else {
        return;
    };
    if let Some(node) = graph.traversal_mut().v(function_node_id).first_mut() {
        node.insert_component_without_rendering_it(FunctionNodePromotedToItsOwnProcessComponent {
            process_execution,
            how_its_own_helper_ran_it,
        });
    }
}

/// The nodes outside any island whose read evaluates `function_node`, through
/// the function nodes `runs_inline` keeps downstream of it.
fn the_readers_evaluating(
    graph: &Graph,
    function_node: &ProcessorUniqueId,
    runs_inline: &dyn Fn(&ProcessorUniqueId) -> bool,
    processors_to_remove: &[ProcessorUniqueId],
) -> BTreeSet<ProcessorUniqueId> {
    let mut readers = BTreeSet::new();
    for downstream in the_function_nodes_downstream_of(graph, function_node, runs_inline) {
        for link in graph.traversal().e(()).iter() {
            let destination = link.to_port().processor_id();
            if link.source().processor_id() == &downstream
                && !runs_inline(destination)
                && !processors_to_remove.contains(destination)
            {
                readers.insert(destination.clone());
            }
        }
    }
    readers
}

/// `function_node` and every function node `runs_inline` keeps that its
/// outputs reach.
fn the_function_nodes_downstream_of(
    graph: &Graph,
    function_node: &ProcessorUniqueId,
    runs_inline: &dyn Fn(&ProcessorUniqueId) -> bool,
) -> HashSet<ProcessorUniqueId> {
    let mut reached = HashSet::new();
    let mut to_follow = vec![function_node.clone()];
    while let Some(following) = to_follow.pop() {
        if !reached.insert(following.clone()) {
            continue;
        }
        for link in graph.traversal().e(()).iter() {
            let destination = link.to_port().processor_id();
            if link.source().processor_id() == &following && runs_inline(destination) {
                to_follow.push(destination.clone());
            }
        }
    }
    reached
}

/// What a promotion adds to the compile that carries it.
#[derive(Debug, Default)]
pub(crate) struct FunctionNodePromotionPlanned {
    /// Each promoted function node, now prepared and spawned as a processor.
    pub(crate) processors_to_add: Vec<ProcessorUniqueId>,
    /// Each link into or out of a promoted node, wired again by what its two
    /// ends now are.
    pub(crate) links_to_add: Vec<LinkUniqueId>,
    /// What `graph` renders under each promoted node's `metrics` once wired.
    pub(crate) promotion_of_each_node: Vec<(
        ProcessorUniqueId,
        FunctionNodePromotionOutOfItsReadersIslands,
    )>,
}

/// What the compile that carries a promotion removes from the graph.
pub(crate) struct WhatTheCompileCarryingAPromotionRemoves<'a> {
    pub(crate) processors: &'a [ProcessorUniqueId],
    pub(crate) links: &'a [LinkUniqueId],
}

/// Move `crashed_function_node` and every function node downstream of it out
/// of `reader`'s islands, marking each to be built as a processor of its own
/// and each of their links to be wired again. Refused, changing nothing, when
/// one of them also feeds a reader other than `reader`, since a running reader
/// is not re-islanded in this release, or when the same compile removes the
/// reader, a node that would move, or one of their links.
pub(crate) fn plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
    graph: &mut Graph,
    crashed_function_node: &ProcessorUniqueId,
    reader: &ProcessorUniqueId,
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
        .filter(|link| promoted.contains(link.source().processor_id()))
        .map(|link| link.to_port().processor_id())
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
            promoted.contains(link.source().processor_id())
                || promoted.contains(link.to_port().processor_id())
        })
        .map(|link| (link.id.clone(), link.to_port().processor_id() == reader))
        .collect();
    let removed_by_the_same_compile = promoted
        .iter()
        .chain(std::iter::once(reader))
        .any(|node_id| what_the_same_compile_removes.processors.contains(node_id))
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
        planned.processors_to_add.push(promoted_id);
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
        if let Some(metrics) = node.get_mut::<ProcessorMetrics>() {
            metrics.promotion_out_of_its_readers_islands = Some(promotion.clone());
        } else {
            node.insert(ProcessorMetrics {
                promotion_out_of_its_readers_islands: Some(promotion.clone()),
                ..Default::default()
            });
        }
    }
}

/// The name `node_id` carries in the graph, or its id when no node has it.
fn the_name_in_the_graph_of(graph: &Graph, node_id: &ProcessorUniqueId) -> String {
    graph
        .traversal()
        .v(node_id)
        .first()
        .map(|node| node.display_name.clone())
        .unwrap_or_else(|| node_id.to_string())
}

/// `function_node` and every function node its outputs reach inside islands.
fn the_function_nodes_from(
    graph: &Graph,
    function_node: &ProcessorUniqueId,
) -> HashSet<ProcessorUniqueId> {
    the_function_nodes_downstream_of(graph, function_node, &|node_id| {
        runs_inside_the_read_of_each_node_it_feeds(graph, node_id)
    })
}

fn the_promoted_nodes_in_a_stable_order(
    promoted: &HashSet<ProcessorUniqueId>,
) -> Vec<ProcessorUniqueId> {
    let mut in_order: Vec<ProcessorUniqueId> = promoted.iter().cloned().collect();
    in_order.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    in_order
}

#[cfg(test)]
mod tests;
