// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::NodeInstanceWithItsOutOfProcessLinkWiring;
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::test_support::{MockOutputOnlyNode, ensure_test_mocks_registered};

fn graph_with_an_instantiated_node_that_declares_no_config() -> (RwLock<Graph>, NodeRunId) {
    ensure_test_mocks_registered();
    let mut graph = Graph::new();
    let node = graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            MockOutputOnlyNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first_mut()
        .expect("the mock is registered");
    let node_instance = NODE_REGISTRY
        .create(node)
        .expect("the mock constructs from its node");
    NodeInstanceWithItsOutOfProcessLinkWiring::from(node_instance).attach_to(node);
    let node_run_id = node.id.clone();
    (RwLock::new(graph), node_run_id)
}

fn config_on_the_node(graph: &RwLock<Graph>, node_run_id: &NodeRunId) -> Option<serde_json::Value> {
    graph
        .read()
        .traversal()
        .v(node_run_id)
        .first()
        .expect("the node is in the graph")
        .config
        .clone()
}

#[test]
fn a_configuration_the_node_refuses_never_reaches_its_graph_node() {
    let (graph, node_run_id) = graph_with_an_instantiated_node_that_declares_no_config();

    let refusal = apply_node_config_update(&graph, &node_run_id, serde_json::json!({"gain": 3}))
        .expect_err("a node declaring no config refuses a key");

    assert!(
        refusal.to_string().contains("gain"),
        "the refusal names the key that had nowhere to go: {refusal}"
    );
    assert_eq!(
        config_on_the_node(&graph, &node_run_id),
        Some(serde_json::Value::Null)
    );
}

#[test]
fn a_configuration_the_node_takes_is_recorded_on_its_graph_node() {
    let (graph, node_run_id) = graph_with_an_instantiated_node_that_declares_no_config();

    let outcome = apply_node_config_update(&graph, &node_run_id, serde_json::json!({}))
        .expect("an empty configuration is taken");

    assert_eq!(outcome, NodeConfigUpdateOutcome::TakenAndRecordedOnTheNode);

    assert_eq!(
        config_on_the_node(&graph, &node_run_id),
        Some(serde_json::json!({}))
    );
}

#[test]
fn an_update_for_a_node_no_longer_in_the_graph_is_passed_over_not_counted_as_taken() {
    let (graph, _node_run_id) = graph_with_an_instantiated_node_that_declares_no_config();

    let outcome = apply_node_config_update(
        &graph,
        &"P-removed-before-the-commit".into(),
        serde_json::json!({}),
    )
    .expect("a node that left the graph is not an error");

    assert_eq!(outcome, NodeConfigUpdateOutcome::NodeNoLongerInTheGraph);
}
