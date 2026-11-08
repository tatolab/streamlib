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
use crate::core::ProcessorUniqueId;
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, FunctionNodePromotionOutOfItsReadersIslands,
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkState, LinkStateComponent,
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
mod tests {
    use serde_json::Value as JsonValue;

    use super::*;
    use crate::core::compiler::compiler_ops::function_node_island::tests::{
        a_function_node_type, a_graph_resolving_these_function_node_types,
        a_graph_resolving_three_function_node_types, a_helper_reader_before_its_setup,
        a_helper_reader_before_its_setup_driven_in, add_a_node, link, reader_setup_command_ports,
    };
    use crate::core::compiler::compiler_ops::function_node_island::{
        HowALinkIsWired, how_a_link_is_wired,
        wire_a_link_from_a_function_node_island_into_its_reader,
    };
    use crate::core::graph::{
        FunctionNodeRanCompiledComponent, HowItsHelpersRanAFunctionNode, JsonSerializableComponent,
    };

    const NOTHING_REMOVED: WhatTheCompileCarryingAPromotionRemoves<'static> =
        WhatTheCompileCarryingAPromotionRemoves {
            processors: &[],
            links: &[],
        };

    fn the_link_state_of(graph: &Graph, link_id: &LinkUniqueId) -> Option<LinkState> {
        graph
            .traversal()
            .e(link_id)
            .first()
            .and_then(|link| link.get::<LinkStateComponent>())
            .map(|state| state.0)
    }

    fn the_process_execution_promoted_to(
        graph: &Graph,
        node_id: &ProcessorUniqueId,
    ) -> Option<ProcessExecution> {
        graph
            .traversal()
            .v(node_id)
            .first()
            .and_then(|node| node.get::<FunctionNodePromotedToItsOwnProcessComponent>())
            .map(|promotion| promotion.process_execution)
    }

    /// `lfo` feeds `remap`, which feeds a reader's helper as one island.
    struct OneIslandIntoOneReader {
        graph: Graph,
        lfo: ProcessorUniqueId,
        remap: ProcessorUniqueId,
        reader: ProcessorUniqueId,
        lfo_into_remap: LinkUniqueId,
        remap_into_the_reader: LinkUniqueId,
    }

    fn one_island_into_one_reader_driven_in(
        reader_process_execution: ProcessExecution,
    ) -> OneIslandIntoOneReader {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let reader =
            a_helper_reader_before_its_setup_driven_in(&mut graph, reader_process_execution);
        let lfo_into_remap = link(&mut graph, (&lfo, "result"), (&remap, "x"));
        let remap_into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
        if reader_process_execution == ProcessExecution::Reactive {
            let source_that_wakes_the_reader = add_a_node(
                &mut graph,
                crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                    .as_str(),
                JsonValue::Null,
            );
            link(
                &mut graph,
                (&source_that_wakes_the_reader, "out1"),
                (&reader, "in1"),
            );
        }
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &remap_into_the_reader,
            &crate::iceoryx2::Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();
        OneIslandIntoOneReader {
            graph,
            lfo,
            remap,
            reader,
            lfo_into_remap,
            remap_into_the_reader,
        }
    }

    #[test]
    fn a_crashed_function_node_leaves_its_readers_island_and_what_it_reads_becomes_its_own_island()
    {
        let OneIslandIntoOneReader {
            mut graph,
            lfo,
            remap,
            reader,
            lfo_into_remap,
            remap_into_the_reader,
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 25 });
        assert_eq!(
            reader_setup_command_ports(&graph, &reader)["islands"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );

        let planned = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &remap,
            &reader,
            "function node `remap` crashed its helper process during its call.",
            &NOTHING_REMOVED,
        )
        .unwrap();

        assert_eq!(planned.processors_to_add, vec![remap.clone()]);
        assert_eq!(
            planned.links_to_add.iter().collect::<HashSet<_>>(),
            HashSet::from([&lfo_into_remap, &remap_into_the_reader])
        );
        assert_eq!(
            reader_setup_command_ports(&graph, &reader)["islands"],
            serde_json::json!([]),
            "the reader's next helper evaluates no island"
        );
        assert_eq!(
            the_process_execution_promoted_to(&graph, &remap),
            Some(ProcessExecution::Continuous { interval_ms: 25 }),
            "the promoted node runs at its reader's frame interval"
        );
        assert!(!runs_inside_the_read_of_each_node_it_feeds(&graph, &remap));
        assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
        assert_eq!(
            how_a_link_is_wired(&graph, &lfo_into_remap).unwrap(),
            HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead
        );
        assert_eq!(
            how_a_link_is_wired(&graph, &remap_into_the_reader).unwrap(),
            HowALinkIsWired::ThroughAChannel
        );
        for link_id in [&lfo_into_remap, &remap_into_the_reader] {
            assert_eq!(the_link_state_of(&graph, link_id), Some(LinkState::Pending));
        }
    }

    #[test]
    fn every_function_node_downstream_of_the_crashed_one_moves_with_it() {
        let OneIslandIntoOneReader {
            mut graph,
            lfo,
            remap,
            reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });

        let planned = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &lfo,
            &reader,
            "function node `lfo` stalled.",
            &NOTHING_REMOVED,
        )
        .unwrap();

        assert_eq!(
            planned.processors_to_add.iter().collect::<HashSet<_>>(),
            HashSet::from([&lfo, &remap])
        );
        assert!(!runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
        assert!(!runs_inside_the_read_of_each_node_it_feeds(&graph, &remap));
    }

    #[test]
    fn a_promotion_out_of_a_reader_with_no_frame_interval_runs_at_the_default_interval() {
        let OneIslandIntoOneReader {
            mut graph,
            remap,
            reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Reactive);

        plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &remap,
            &reader,
            "function node `remap` stalled.",
            &NOTHING_REMOVED,
        )
        .unwrap();

        assert_eq!(
            the_process_execution_promoted_to(&graph, &remap),
            Some(ProcessExecution::Continuous {
                interval_ms: PROMOTED_FUNCTION_NODE_INTERVAL_MS_FOR_A_READER_WITH_NONE
            })
        );
    }

    #[test]
    fn a_promotion_that_would_re_island_a_second_running_reader_is_refused_changing_nothing() {
        let OneIslandIntoOneReader {
            mut graph,
            remap,
            reader,
            remap_into_the_reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
        let second_reader = a_helper_reader_before_its_setup_driven_in(
            &mut graph,
            ProcessExecution::Continuous { interval_ms: 10 },
        );
        link(&mut graph, (&remap, "result"), (&second_reader, "in1"));

        let refusal = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &remap,
            &reader,
            "function node `remap` stalled.",
            &NOTHING_REMOVED,
        )
        .unwrap_err()
        .to_string();

        let second_reader_name = the_name_in_the_graph_of(&graph, &second_reader);
        assert!(
            refusal.contains(&format!("`{second_reader_name}`")),
            "{refusal}"
        );
        assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &remap));
        assert_eq!(
            reader_setup_command_ports(&graph, &reader)["islands"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            the_link_state_of(&graph, &remap_into_the_reader),
            Some(LinkState::Wired)
        );
    }

    #[test]
    fn a_promotion_whose_link_the_same_compile_removes_is_refused_changing_nothing() {
        let OneIslandIntoOneReader {
            mut graph,
            lfo,
            reader,
            lfo_into_remap,
            remap_into_the_reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });

        let refusal = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &lfo,
            &reader,
            "function node `lfo` stalled.",
            &WhatTheCompileCarryingAPromotionRemoves {
                processors: &[],
                links: std::slice::from_ref(&lfo_into_remap),
            },
        )
        .unwrap_err()
        .to_string();

        assert!(refusal.contains("the same change removes"), "{refusal}");
        assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
        assert_eq!(
            the_link_state_of(&graph, &remap_into_the_reader),
            Some(LinkState::Wired)
        );
    }

    #[test]
    fn a_link_into_a_promoted_function_node_from_a_node_that_is_not_one_is_refused() {
        let OneIslandIntoOneReader {
            mut graph,
            remap,
            reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
        plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &remap,
            &reader,
            "function node `remap` stalled.",
            &NOTHING_REMOVED,
        )
        .unwrap();
        let process_node = add_a_node(
            &mut graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );

        let into_the_promoted_node = link(&mut graph, (&process_node, "out1"), (&remap, "x"));

        let refusal = how_a_link_is_wired(&graph, &into_the_promoted_node)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("which is not a function node"),
            "{refusal}"
        );
    }

    #[test]
    fn a_function_node_already_out_of_its_readers_islands_is_not_promoted_again() {
        let OneIslandIntoOneReader {
            mut graph,
            remap,
            reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
        plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &remap,
            &reader,
            "first",
            &NOTHING_REMOVED,
        )
        .unwrap();

        let refusal = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &remap,
            &reader,
            "second",
            &NOTHING_REMOVED,
        )
        .unwrap_err()
        .to_string();

        assert!(
            refusal.contains("is not a function node inside"),
            "{refusal}"
        );
    }

    #[test]
    fn graph_shows_each_promotion_under_the_promoted_nodes_metrics() {
        let OneIslandIntoOneReader {
            mut graph,
            lfo,
            remap,
            reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
        let planned = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &lfo,
            &reader,
            "function node `lfo` stalled.",
            &NOTHING_REMOVED,
        )
        .unwrap();

        record_each_promotion_on_its_nodes_metrics(&mut graph, &planned);

        for promoted_id in [&lfo, &remap] {
            let metrics = graph
                .traversal()
                .v(promoted_id)
                .first()
                .and_then(|node| node.get::<ProcessorMetrics>())
                .map(JsonSerializableComponent::to_json)
                .unwrap();
            assert_eq!(
                metrics["promoted_out_of_its_readers_islands"],
                serde_json::json!({
                    "promotions": 1,
                    "after_crashing_the_helper_of": the_name_in_the_graph_of(&graph, &reader),
                    "reason": "function node `lfo` stalled.",
                }),
                "{metrics}"
            );
        }
    }

    /// A graph resolving `lfo`, `remap` and `mix` with no state, and `counter`
    /// and `second_counter`, which carry one.
    fn a_graph_resolving_two_stateful_function_node_types() -> Graph {
        let stateful = |import_path| {
            let mut descriptor = a_function_node_type(import_path, &[], &["result"]);
            descriptor.function_node_state_fields = vec!["calls".to_string()];
            descriptor
        };
        a_graph_resolving_these_function_node_types([
            a_function_node_type("test_ops:lfo", &[], &["result"]),
            a_function_node_type("test_ops:remap", &["x"], &["result"]),
            a_function_node_type("test_ops:mix", &["a", "b"], &["result"]),
            stateful("test_ops:counter"),
            stateful("test_ops:second_counter"),
        ])
    }

    fn every_node_of(graph: &Graph) -> Vec<ProcessorUniqueId> {
        graph.traversal().v(()).ids()
    }

    #[test]
    fn a_stateful_node_two_readers_read_moves_out_of_the_islands_with_what_it_feeds() {
        let mut graph = a_graph_resolving_two_stateful_function_node_types();
        let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let first_reader = a_helper_reader_before_its_setup(&mut graph);
        let second_reader = a_helper_reader_before_its_setup(&mut graph);
        let counter_into_the_first_reader =
            link(&mut graph, (&counter, "result"), (&first_reader, "in1"));
        link(&mut graph, (&counter, "result"), (&remap, "x"));
        link(&mut graph, (&remap, "result"), (&second_reader, "in1"));
        let added = every_node_of(&graph);

        place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
            &mut graph,
            &added,
            &[],
        )
        .unwrap();

        for moved in [&counter, &remap] {
            assert_eq!(
                the_process_execution_promoted_to(&graph, moved),
                Some(ProcessExecution::Continuous {
                    interval_ms: STATEFUL_FUNCTION_NODE_SEVERAL_READERS_READ_INTERVAL_MS
                })
            );
        }
        assert_eq!(
            how_a_link_is_wired(&graph, &counter_into_the_first_reader).unwrap(),
            HowALinkIsWired::ThroughAChannel
        );
    }

    fn the_ran_compiled_graph_renders_on(graph: &Graph, node_id: &ProcessorUniqueId) -> JsonValue {
        graph
            .traversal()
            .v(node_id)
            .first()
            .and_then(|node| node.get::<FunctionNodeRanCompiledComponent>())
            .map(|component| component.to_json())
            .expect("a promoted function node carries the report `graph` renders")
    }

    fn the_report_its_own_helper_notes_on(
        graph: &Graph,
        node_id: &ProcessorUniqueId,
    ) -> std::sync::Arc<HowItsHelpersRanAFunctionNode> {
        graph
            .traversal()
            .v(node_id)
            .first()
            .and_then(|node| node.get::<FunctionNodePromotedToItsOwnProcessComponent>())
            .map(|promotion| std::sync::Arc::clone(&promotion.how_its_own_helper_ran_it))
            .expect("the node was promoted")
    }

    /// A stateful node several readers read is evaluated by no reader, so the
    /// helper of its own is what reports how it ran it.
    ///
    /// Fail-without-fix: the node carries no report its own helper notes on,
    /// and `graph` shows `ran_compiled: null` for as long as it runs.
    #[test]
    fn a_stateful_node_two_readers_read_shows_how_its_own_helper_ran_it() {
        let mut graph = a_graph_resolving_two_stateful_function_node_types();
        let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
        let first_reader = a_helper_reader_before_its_setup(&mut graph);
        let second_reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&counter, "result"), (&first_reader, "in1"));
        link(&mut graph, (&counter, "result"), (&second_reader, "in1"));
        let added = every_node_of(&graph);

        place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
            &mut graph,
            &added,
            &[],
        )
        .unwrap();

        assert_eq!(
            the_ran_compiled_graph_renders_on(&graph, &counter),
            JsonValue::Null,
            "null until its own helper has set up"
        );
        the_report_its_own_helper_notes_on(&graph, &counter).note_it_ran_compiled(true);
        assert_eq!(
            the_ran_compiled_graph_renders_on(&graph, &counter),
            JsonValue::Bool(true)
        );
    }

    /// A node promoted after a crash shows how its reader ran it until its own
    /// helper reports, then how its own helper runs it.
    #[test]
    fn a_node_promoted_out_of_its_reader_shows_how_its_own_helper_runs_it() {
        let OneIslandIntoOneReader {
            mut graph,
            remap,
            reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 25 });
        graph
            .traversal()
            .v(&remap)
            .first()
            .and_then(|node| node.get::<FunctionNodeRanCompiledComponent>())
            .expect("its reader's island attached the report")
            .0
            .note_it_ran_compiled(false);

        plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph,
            &remap,
            &reader,
            "function node `remap` crashed its helper process during its call.",
            &NOTHING_REMOVED,
        )
        .unwrap();

        assert_eq!(
            the_ran_compiled_graph_renders_on(&graph, &remap),
            JsonValue::Bool(false),
            "how its reader ran it, until its own helper reports"
        );
        the_report_its_own_helper_notes_on(&graph, &remap).note_it_ran_compiled(true);
        assert_eq!(
            the_ran_compiled_graph_renders_on(&graph, &remap),
            JsonValue::Bool(true)
        );
    }

    #[test]
    fn a_stateful_node_one_reader_reads_and_a_stateless_one_two_read_stay_inline() {
        let mut graph = a_graph_resolving_two_stateful_function_node_types();
        let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let counters_reader = a_helper_reader_before_its_setup(&mut graph);
        let first_lfo_reader = a_helper_reader_before_its_setup(&mut graph);
        let second_lfo_reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&counter, "result"), (&counters_reader, "in1"));
        link(&mut graph, (&lfo, "result"), (&first_lfo_reader, "in1"));
        link(&mut graph, (&lfo, "result"), (&second_lfo_reader, "in1"));
        let added = every_node_of(&graph);

        place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
            &mut graph,
            &added,
            &[],
        )
        .unwrap();

        assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &counter));
        assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
    }

    #[test]
    fn a_reader_removed_in_the_same_compile_does_not_count_as_one_of_its_readers() {
        let mut graph = a_graph_resolving_two_stateful_function_node_types();
        let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
        let kept_reader = a_helper_reader_before_its_setup(&mut graph);
        let removed_reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&counter, "result"), (&kept_reader, "in1"));
        link(&mut graph, (&counter, "result"), (&removed_reader, "in1"));

        place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
            &mut graph,
            &[counter.clone(), kept_reader],
            &[removed_reader],
        )
        .unwrap();

        assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &counter));
    }

    #[test]
    fn a_stateful_node_that_reads_a_moved_one_beside_another_reader_moves_too() {
        let mut graph = a_graph_resolving_two_stateful_function_node_types();
        let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
        let second_counter = add_a_node(&mut graph, "test_ops:second_counter", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let mix_reader = a_helper_reader_before_its_setup(&mut graph);
        let counter_reader = a_helper_reader_before_its_setup(&mut graph);
        let second_counter_reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&counter, "result"), (&mix, "a"));
        link(&mut graph, (&second_counter, "result"), (&mix, "b"));
        link(&mut graph, (&mix, "result"), (&mix_reader, "in1"));
        link(&mut graph, (&counter, "result"), (&counter_reader, "in1"));
        link(
            &mut graph,
            (&second_counter, "result"),
            (&second_counter_reader, "in1"),
        );
        let added = every_node_of(&graph);

        place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
            &mut graph,
            &added,
            &[],
        )
        .unwrap();

        for moved in [&counter, &mix, &second_counter] {
            assert!(
                !runs_inside_the_read_of_each_node_it_feeds(&graph, moved),
                "`mix` moved with `counter`, so `second_counter` is read by `mix`'s helper and \
                 its own reader"
            );
        }
    }

    #[test]
    fn a_second_reader_of_a_stateful_node_a_running_reader_evaluates_is_refused_naming_it() {
        let mut graph = a_graph_resolving_two_stateful_function_node_types();
        let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
        let running_reader = a_helper_reader_before_its_setup(&mut graph);
        let new_reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&counter, "result"), (&running_reader, "in1"));
        link(&mut graph, (&counter, "result"), (&new_reader, "in1"));
        let running_reader_name = the_name_in_the_graph_of(&graph, &running_reader);

        let refusal =
            place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
                &mut graph,
                &[counter.clone(), new_reader],
                &[],
            )
            .unwrap_err()
            .to_string();

        assert!(
            refusal.contains(&format!("`{running_reader_name}` is already running")),
            "{refusal}"
        );
        assert!(refusal.contains("re-islanded"), "{refusal}");
        assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &counter));
    }
}
