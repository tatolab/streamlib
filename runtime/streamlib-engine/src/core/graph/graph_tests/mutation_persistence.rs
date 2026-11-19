// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_component_mutation_persists() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();

    graph
        .traversal_mut()
        .v(id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockState(NodeState::Idle));

    {
        let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
        if let Some(state) = node.get_mut::<MockState>() {
            state.0 = NodeState::Running;
        }
    }

    let state = graph
        .traversal()
        .v(id.as_str())
        .first()
        .unwrap()
        .get::<MockState>()
        .unwrap();

    assert_eq!(state.0, NodeState::Running);
}

#[test]
fn test_arc_shared_component_modification() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();

    let gate = MockPauseGate::new();
    let gate_clone = gate.clone();

    graph
        .traversal_mut()
        .v(id.as_str())
        .first_mut()
        .unwrap()
        .insert(gate);

    gate_clone.pause();

    let stored_gate = graph
        .traversal()
        .v(id.as_str())
        .first()
        .unwrap()
        .get::<MockPauseGate>()
        .unwrap();

    assert!(stored_gate.is_paused());
}

#[test]
fn test_drop_removes_node() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();

    assert!(graph.traversal().v(id.as_str()).exists());

    graph.traversal_mut().v(id.as_str()).drop();

    assert!(!graph.traversal().v(id.as_str()).exists());
}

#[test]
fn test_drop_removes_link() {
    let mut graph = test_graph();

    let upstream_id = graph
        .traversal_mut()
        .add_v(MockOutputOnlyNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();
    let downstream_id = graph
        .traversal_mut()
        .add_v(MockInputOnlyNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();
    let link_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&upstream_id, "out1"),
            InputLinkPortRef::new(&downstream_id, "in1"),
        )
        .first()
        .expect("link should be created")
        .id
        .to_string();

    assert!(graph.traversal().e(link_id.as_str()).exists());

    graph.traversal_mut().e(link_id.as_str()).drop();

    assert!(!graph.traversal().e(link_id.as_str()).exists());
}

/// A node dropped while another under its UUID replaced it leaves the values set live on the
/// replacement's inputs, and forgets them once no node holds the UUID.
///
/// Mental-revert: forgetting every value of the UUID as the old node leaves takes off the value
/// set live on the replacement in the same commit.
#[test]
fn a_node_dropped_after_its_replacement_leaves_the_replacements_live_values() {
    use crate::core::descriptors::NodeValueLifetime;
    use crate::core::graph::WhyTheNodeLeaves;
    use crate::core::test_support::add_mock_input_only;

    ensure_test_mocks_registered();
    let mut graph = Graph::new();
    let leaving: crate::core::graph::NodeRunId = add_mock_input_only(&mut graph).as_str().into();
    let replacement: crate::core::graph::NodeRunId =
        add_mock_input_only(&mut graph).as_str().into();
    let node_uuid = graph.traversal().v(&leaving).first().unwrap().node_uuid;
    graph
        .traversal_mut()
        .v(&replacement)
        .first_mut()
        .unwrap()
        .node_uuid = node_uuid;
    graph.live_input_values().set(
        node_uuid,
        "in1",
        Some(serde_json::json!(2)),
        NodeValueLifetime::Reset,
    );

    assert!(graph.drop_the_node(&leaving, WhyTheNodeLeaves::Removed));
    assert_eq!(
        graph.live_input_values().value_of(node_uuid, "in1"),
        Some(serde_json::json!(2))
    );

    assert!(graph.drop_the_node(&replacement, WhyTheNodeLeaves::Removed));
    assert_eq!(graph.live_input_values().value_of(node_uuid, "in1"), None);
}
