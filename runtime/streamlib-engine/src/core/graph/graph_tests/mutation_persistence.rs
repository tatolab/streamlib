// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Mutation Persistence Tests.

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_component_mutation_persists() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();

    graph
        .traversal_mut()
        .v(id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockState(ProcessorState::Idle));

    {
        let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
        if let Some(state) = node.get_mut::<MockState>() {
            state.0 = ProcessorState::Running;
        }
    }

    let state = graph
        .traversal()
        .v(id.as_str())
        .first()
        .unwrap()
        .get::<MockState>()
        .unwrap();

    assert_eq!(state.0, ProcessorState::Running);
}

#[test]
fn test_arc_shared_component_modification() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
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
fn test_drop_removes_processor() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
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
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let downstream_id = graph
        .traversal_mut()
        .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
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
