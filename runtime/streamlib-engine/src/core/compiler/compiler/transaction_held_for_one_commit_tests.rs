// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::compiler::compiler_ops::shader_node_island::ShaderNodeFusedIntoTheIslandOfAnotherComponent;
use crate::core::graph::NodeRunId;
use crate::core::test_support::add_mock_output_only;

fn the_node_is_in(compiler: &Compiler, node_run_id: &NodeRunId) -> bool {
    compiler.scope_reading(|graph| graph.traversal().v(node_run_id).first().is_some())
}

/// An added node a commit fused into the island another node runs is built, though it holds
/// no instance of its own, so a take-back leaves it in the graph, and an added node no commit
/// built leaves.
///
/// Mental-revert: judging a node built only by an instance of its own drops the fused node,
/// and the island its host runs keeps a member the graph no longer holds.
#[test]
fn a_take_back_keeps_an_added_node_a_commit_placed_in_an_island() {
    let compiler = Compiler::new();
    let (fused, unbuilt): (NodeRunId, NodeRunId) = compiler.scope(|graph, _tx| {
        let fused: NodeRunId = add_mock_output_only(graph).as_str().into();
        let unbuilt: NodeRunId = add_mock_output_only(graph).as_str().into();
        graph
            .traversal_mut()
            .v(&fused)
            .first_mut()
            .expect("the node was added")
            .insert_component_without_rendering_it(ShaderNodeFusedIntoTheIslandOfAnotherComponent);
        (fused, unbuilt)
    });

    compiler.take_back_what_operations_did_to_the_graph(&[
        PendingOperation::AddNode(fused.clone()),
        PendingOperation::AddNode(unbuilt.clone()),
    ]);

    assert!(the_node_is_in(&compiler, &fused));
    assert!(!the_node_is_in(&compiler, &unbuilt));
}
