// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::test_support::{MockOutputOnlyNode, ensure_test_mocks_registered};

fn the_pause_gate_a_node_is_prepared_with_while_its_graph_is(graph_state: GraphState) -> bool {
    ensure_test_mocks_registered();
    let mut graph = Graph::new();
    graph
        .traversal_mut()
        .add_v(MockOutputOnlyNode::Node::node(Default::default()))
        .expect("the node is added");
    let node_run_id = graph.traversal().v(()).ids().remove(0);
    graph.set_state(graph_state);

    let _barrier = prepare_node(
        &mut graph,
        &node_run_id,
        &crate::core::pubsub::LoadedStreamIdentity {
            runtime_id: "prepare-node-test".to_string(),
            stream_id: "main".to_string(),
            stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
        },
    )
    .expect("the node is prepared");

    graph
        .traversal()
        .v(&node_run_id)
        .first()
        .and_then(|node| node.get::<NodePauseGateComponent>())
        .expect("the node holds a pause gate")
        .is_paused()
}

#[test]
fn a_node_prepared_while_its_stream_is_paused_starts_paused() {
    assert!(the_pause_gate_a_node_is_prepared_with_while_its_graph_is(
        GraphState::Paused
    ));
    assert!(!the_pause_gate_a_node_is_prepared_with_while_its_graph_is(
        GraphState::Running
    ));
}
