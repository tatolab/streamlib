// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_class_node_feeding_remap_into_a_helper_reader, a_graph_resolving_three_function_node_types,
    add_a_node, link,
};
use crate::core::compiler::compiler_ops::function_node_island::wire_a_link_from_a_function_node_island_into_its_reader;
use crate::core::graph::LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent;
use crate::core::test_support::MockNode;
use crate::iceoryx2::Iceoryx2Node;

fn a_mock_node(graph: &mut Graph) -> NodeRunId {
    add_a_node(
        graph,
        MockNode::node_class_import_path().as_str(),
        JsonValue::Null,
    )
}

fn sorted<T: Ord + Clone>(items: &[T]) -> Vec<T> {
    let mut items = items.to_vec();
    items.sort();
    items
}

#[test]
fn a_rebuilt_node_restarts_with_each_link_into_and_out_of_it() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let upstream = a_mock_node(&mut graph);
    let rebuilt = a_mock_node(&mut graph);
    let downstream = a_mock_node(&mut graph);
    let into_it = link(&mut graph, (&upstream, "out1"), (&rebuilt, "in1"));
    let out_of_it = link(&mut graph, (&rebuilt, "out1"), (&downstream, "in1"));
    let elsewhere = link(&mut graph, (&upstream, "out2"), (&downstream, "in2"));

    let restarted =
        the_running_nodes_a_rebuild_restarts(&graph, std::slice::from_ref(&rebuilt), &[], &[], &[])
            .unwrap();

    assert_eq!(
        restarted,
        RunningNodesALiveEditRestarts {
            nodes: vec![rebuilt],
            links: sorted(&[into_it, out_of_it]),
        }
    );
    assert!(!restarted.links.contains(&elsewhere));
}

#[test]
fn a_node_or_link_the_same_edit_adds_or_removes_is_not_restarted() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let upstream = a_mock_node(&mut graph);
    let rebuilt = a_mock_node(&mut graph);
    let removed = a_mock_node(&mut graph);
    let added = a_mock_node(&mut graph);
    let kept = link(&mut graph, (&upstream, "out1"), (&rebuilt, "in1"));
    let being_added = link(&mut graph, (&upstream, "out2"), (&rebuilt, "in2"));
    link(&mut graph, (&rebuilt, "out1"), (&removed, "in1"));

    let restarted = the_running_nodes_a_rebuild_restarts(
        &graph,
        &[rebuilt.clone(), removed.clone(), added.clone()],
        std::slice::from_ref(&added),
        std::slice::from_ref(&removed),
        std::slice::from_ref(&being_added),
    )
    .unwrap();

    assert_eq!(
        restarted,
        RunningNodesALiveEditRestarts {
            nodes: vec![rebuilt],
            links: vec![kept],
        }
    );
}

/// Mark `function_node` promoted out of its readers' islands into a helper of its own.
fn promote(graph: &mut Graph, function_node: &NodeRunId) {
    graph
        .traversal_mut()
        .v(function_node)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(
            crate::core::graph::FunctionNodePromotedToItsOwnProcessComponent {
                process_execution: crate::core::execution::ProcessExecution::Continuous {
                    interval_ms: 10,
                },
                how_its_own_helper_ran_it: std::sync::Arc::default(),
                evaluated_at_setup: true,
            },
        );
}

/// A node whose setting a wire rebuilds restarts with an island into another of its inputs,
/// which it evaluates itself and is fed again as it sets up, and with a channel from a function
/// node promoted out of the islands; the island is taken off it with no channel to close.
///
/// Mental-revert: refusing every link that joins a function node refuses the rebuild, and the
/// setting its wire carries a value into never takes it.
#[test]
fn a_rebuild_restarts_a_node_with_the_island_it_evaluates_and_a_promoted_nodes_channel() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let promoted = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    promote(&mut graph, &promoted);
    let rebuilt = a_mock_node(&mut graph);
    let island_it_evaluates = link(&mut graph, (&lfo, "result"), (&rebuilt, "in1"));
    let from_the_promoted_node = link(&mut graph, (&promoted, "result"), (&rebuilt, "in2"));

    let restarted =
        the_running_nodes_a_rebuild_restarts(&graph, std::slice::from_ref(&rebuilt), &[], &[], &[])
            .unwrap();
    unwire_each_link_of_the_nodes_restarted(&mut graph, &restarted).unwrap();

    assert_eq!(
        restarted,
        RunningNodesALiveEditRestarts {
            nodes: vec![rebuilt],
            links: sorted(&[island_it_evaluates.clone(), from_the_promoted_node]),
        }
    );
    assert!(
        graph
            .traversal()
            .e(&island_it_evaluates)
            .first()
            .unwrap()
            .get::<LinkStateComponent>()
            .is_some_and(|state| state.0 == LinkState::Disconnected),
        "the island is taken off its reader, to be fed to it again"
    );
}

/// Whether `link_id`, a channel into an island, is read at the reader of the island `island_link`.
fn is_read_for_the_island_link(
    graph: &Graph,
    link_id: &LinkUniqueId,
    island_link: &LinkUniqueId,
) -> bool {
    graph
        .traversal()
        .e(link_id)
        .first()
        .unwrap()
        .get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
        .is_some_and(|read_through| {
            read_through
                .the_read_for_the_island_link(island_link)
                .is_some()
        })
}

/// A rebuilt node evaluating an island that reads a class node's channel closes that read as it
/// is unwired, the island taken off it, and opens it again as the island is wired into it anew.
///
/// Mental-revert: closing the island link as a channel leaves the class node's channel read at
/// the reader, which its next setup opens a second time.
#[test]
fn a_rebuild_closes_the_read_its_island_holds_of_an_upstream_channel_and_opens_it_again() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (_remap, rebuilt, into_the_island, into_the_rebuilt) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_rebuilt,
        &iceoryx2_node,
    )
    .unwrap();
    assert!(is_read_for_the_island_link(
        &graph,
        &into_the_island,
        &into_the_rebuilt
    ));

    let restarted =
        the_running_nodes_a_rebuild_restarts(&graph, std::slice::from_ref(&rebuilt), &[], &[], &[])
            .unwrap();
    assert_eq!(restarted.links, [into_the_rebuilt.clone()]);
    unwire_each_link_of_the_nodes_restarted(&mut graph, &restarted).unwrap();
    assert!(
        !is_read_for_the_island_link(&graph, &into_the_island, &into_the_rebuilt),
        "the class node's channel is no longer read at the rebuilt node"
    );

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_rebuilt,
        &iceoryx2_node,
    )
    .unwrap();
    assert!(
        is_read_for_the_island_link(&graph, &into_the_island, &into_the_rebuilt),
        "wired again, the island reads the class node's channel at the rebuilt node again"
    );
}

/// A node feeding a function node run in a process of its own is refused a rebuild: that node's
/// channel from it is not wired again with the rebuilt node.
#[test]
fn a_rebuild_of_a_node_feeding_a_promoted_function_node_is_refused() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let rebuilt = a_mock_node(&mut graph);
    let promoted = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    promote(&mut graph, &promoted);
    let reader = a_mock_node(&mut graph);
    link(&mut graph, (&rebuilt, "out1"), (&promoted, "x"));
    link(&mut graph, (&promoted, "result"), (&reader, "in1"));

    let refusal =
        the_running_nodes_a_rebuild_restarts(&graph, std::slice::from_ref(&rebuilt), &[], &[], &[])
            .unwrap_err()
            .to_string();
    assert!(refusal.contains("To fix: stop the stream"), "{refusal}");
}

/// A node whose output feeds an island another node evaluates is refused a rebuild, naming both
/// nodes: its place in that island is read once, when the island's reader starts.
#[test]
fn a_rebuild_of_a_node_feeding_an_island_another_node_evaluates_is_refused_naming_both() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let rebuilt = a_mock_node(&mut graph);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let island_reader = a_mock_node(&mut graph);
    link(&mut graph, (&rebuilt, "out1"), (&remap, "x"));
    link(&mut graph, (&remap, "result"), (&island_reader, "in1"));

    let refusal =
        the_running_nodes_a_rebuild_restarts(&graph, std::slice::from_ref(&rebuilt), &[], &[], &[])
            .unwrap_err()
            .to_string();

    let name_of = |node: &NodeRunId| {
        graph
            .traversal()
            .v(node)
            .first()
            .unwrap()
            .display_name
            .clone()
    };
    assert!(
        refusal.contains(&format!("`{}` is rebuilt", name_of(&rebuilt)))
            && refusal.contains(&format!("joins function node `{}`", name_of(&remap)))
            && refusal.contains("To fix: stop the stream"),
        "{refusal}"
    );
}

#[test]
fn a_rebuild_naming_a_function_node_is_refused() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);

    let refusal =
        the_running_nodes_a_rebuild_restarts(&graph, std::slice::from_ref(&lfo), &[], &[], &[])
            .unwrap_err()
            .to_string();

    assert!(
        refusal.contains("is a function node, which reads no setting at setup"),
        "{refusal}"
    );
}

#[test]
fn two_restarts_joined_hold_each_node_and_link_once() {
    let joined = RunningNodesALiveEditRestarts {
        nodes: vec![NodeRunId::from("Pb"), NodeRunId::from("Pa")],
        links: vec![LinkUniqueId::from("Lb")],
    }
    .joined_with(RunningNodesALiveEditRestarts {
        nodes: vec![NodeRunId::from("Pa")],
        links: vec![LinkUniqueId::from("La"), LinkUniqueId::from("Lb")],
    });

    assert_eq!(
        joined,
        RunningNodesALiveEditRestarts {
            nodes: vec![NodeRunId::from("Pa"), NodeRunId::from("Pb")],
            links: vec![LinkUniqueId::from("La"), LinkUniqueId::from("Lb")],
        }
    );
}
