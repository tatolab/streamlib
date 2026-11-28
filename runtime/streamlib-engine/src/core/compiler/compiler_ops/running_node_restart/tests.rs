// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_class_node_feeding_remap_into_a_helper_reader, a_graph_resolving_three_function_node_types,
};
use crate::core::compiler::compiler_ops::function_node_island::wire_a_link_from_a_function_node_island_into_its_reader;
use crate::core::graph::LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent;
use crate::iceoryx2::Iceoryx2Node;

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

/// A restarted node evaluating an island that reads a class node's channel closes that read as
/// it is unwired, the island taken off it, and opens it again as the island is wired into it
/// anew.
///
/// Mental-revert: closing the island link as a channel leaves the class node's channel read at
/// the reader, which its next setup opens a second time.
#[test]
fn a_restart_closes_the_read_its_island_holds_of_an_upstream_channel_and_opens_it_again() {
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

    let restarted = RunningNodesALiveEditRestarts {
        nodes: vec![rebuilt.clone()],
        links: vec![into_the_rebuilt.clone()],
    };
    unwire_each_link_of_the_nodes_restarted(&mut graph, &restarted).unwrap();
    assert!(
        !is_read_for_the_island_link(&graph, &into_the_island, &into_the_rebuilt),
        "the class node's channel is no longer read at the restarted node"
    );

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_rebuilt,
        &iceoryx2_node,
    )
    .unwrap();
    assert!(
        is_read_for_the_island_link(&graph, &into_the_island, &into_the_rebuilt),
        "wired again, the island reads the class node's channel at the restarted node again"
    );
}
