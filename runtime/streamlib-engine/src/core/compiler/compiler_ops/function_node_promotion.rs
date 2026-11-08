// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Promotion on crash: a function node whose call crashed or stalled its
//! reader's helper moves out of that reader's islands into a helper process of
//! its own, with every function node downstream of it, so no node left inline
//! reads one that moved.

use std::collections::{BTreeSet, HashSet};

use super::function_node_island::runs_inside_the_read_of_each_node_it_feeds;
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

/// Move `crashed_function_node` and every function node downstream of it out
/// of `reader`'s islands, marking each to be built as a processor of its own
/// and each of their links to be wired again; refused, changing nothing, when
/// one of them also feeds a reader other than `reader`, since a running reader
/// is not re-islanded in this release.
pub(crate) fn plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
    graph: &mut Graph,
    crashed_function_node: &ProcessorUniqueId,
    reader: &ProcessorUniqueId,
    reason: &str,
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
        if let Some(node) = graph.traversal_mut().v(&promoted_id).first_mut() {
            node.insert_component_without_rendering_it(
                FunctionNodePromotedToItsOwnProcessComponent { process_execution },
            );
        }
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
    let mut reached = HashSet::new();
    let mut to_follow = vec![function_node.clone()];
    while let Some(following) = to_follow.pop() {
        if !reached.insert(following.clone()) {
            continue;
        }
        for link in graph.traversal().e(()).iter() {
            let destination = link.to_port().processor_id();
            if link.source().processor_id() == &following
                && runs_inside_the_read_of_each_node_it_feeds(graph, destination)
            {
                to_follow.push(destination.clone());
            }
        }
    }
    reached
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
        a_graph_resolving_three_function_node_types, a_helper_reader_before_its_setup_driven_in,
        add_a_node, link, reader_setup_command_ports,
    };
    use crate::core::compiler::compiler_ops::function_node_island::{
        HowALinkIsWired, how_a_link_is_wired,
        wire_a_link_from_a_function_node_island_into_its_reader,
    };
    use crate::core::graph::JsonSerializableComponent;

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
        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &remap_into_the_reader)
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
    fn a_function_node_already_out_of_its_readers_islands_is_not_promoted_again() {
        let OneIslandIntoOneReader {
            mut graph,
            remap,
            reader,
            ..
        } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
        plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph, &remap, &reader, "first",
        )
        .unwrap();

        let refusal = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
            &mut graph, &remap, &reader, "second",
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
}
