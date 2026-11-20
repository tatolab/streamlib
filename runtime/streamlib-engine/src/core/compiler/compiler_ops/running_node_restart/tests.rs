// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_graph_resolving_three_function_node_types, add_a_node, link,
};
use crate::core::test_support::MockNode;

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

#[test]
fn a_rebuild_of_a_node_linked_to_a_function_node_is_refused_naming_both() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let rebuilt = a_mock_node(&mut graph);
    link(&mut graph, (&lfo, "result"), (&rebuilt, "in1"));

    let refusal =
        the_running_nodes_a_rebuild_restarts(&graph, std::slice::from_ref(&rebuilt), &[], &[], &[])
            .unwrap_err()
            .to_string();

    let rebuilt_name = graph
        .traversal()
        .v(&rebuilt)
        .first()
        .unwrap()
        .display_name
        .clone();
    let lfo_name = graph
        .traversal()
        .v(&lfo)
        .first()
        .unwrap()
        .display_name
        .clone();
    assert!(
        refusal.contains(&format!("`{rebuilt_name}` is rebuilt"))
            && refusal.contains(&format!("joins function node `{lfo_name}`"))
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
