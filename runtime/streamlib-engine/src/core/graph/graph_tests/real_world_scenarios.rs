// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_compiler_create_phase_pattern() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();

    {
        let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
        node.insert(MockProcessorInstance);
        node.insert(MockState::default());
        node.insert(MockPauseGate::new());
    }

    let node = graph.traversal().v(id.as_str()).first().unwrap();
    assert!(node.has::<MockProcessorInstance>());
    assert!(node.has::<MockState>());
    assert!(node.has::<MockPauseGate>());
}

#[test]
fn test_compiler_wire_phase_pattern() {
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

    {
        let link = graph
            .traversal_mut()
            .e(link_id.as_str())
            .first_mut()
            .unwrap();
        link.insert(MockLinkInstance::new(4));
    }

    let link = graph.traversal().e(link_id.as_str()).first().unwrap();
    assert!(link.has::<MockLinkInstance>());
}

#[test]
fn test_runtime_pause_all_pattern() {
    let mut graph = test_graph();

    let id1 = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let id2 = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let id3 = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();

    for id in [&id1, &id2, &id3] {
        graph
            .traversal_mut()
            .v(id.as_str())
            .first_mut()
            .unwrap()
            .insert(MockPauseGate::new());
    }

    let ids: Vec<_> = graph.traversal().v(()).ids();
    for id in &ids {
        let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
        if let Some(gate) = node.get::<MockPauseGate>() {
            gate.pause();
        }
    }

    for id in &ids {
        let gate = graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .get::<MockPauseGate>()
            .unwrap();
        assert!(gate.is_paused());
    }
}

#[test]
fn test_runtime_status_pattern() {
    let mut graph = test_graph();

    let id1 = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let id2 = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named");

    graph
        .traversal_mut()
        .v(id1.as_str())
        .first_mut()
        .unwrap()
        .insert(MockState(ProcessorState::Running));

    graph
        .traversal_mut()
        .v(id2.as_str())
        .first_mut()
        .unwrap()
        .insert(MockState(ProcessorState::Paused));

    let processor_states: Vec<(String, ProcessorState)> = graph
        .traversal()
        .v(())
        .has_component::<MockState>()
        .iter()
        .filter_map(|node| {
            let state = node.get::<MockState>()?;
            Some((node.id.to_string(), state.0))
        })
        .collect();

    assert_eq!(processor_states.len(), 2);

    let states_map: std::collections::HashMap<_, _> = processor_states.into_iter().collect();
    assert_eq!(states_map.get(&id1), Some(&ProcessorState::Running));
    assert_eq!(states_map.get(&id2), Some(&ProcessorState::Paused));
}

#[test]
fn test_pipeline_topology() {
    let mut graph = test_graph();

    let upstream_id = graph
        .traversal_mut()
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let middle_id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
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

    graph.traversal_mut().add_e(
        OutputLinkPortRef::new(&upstream_id, "out1"),
        InputLinkPortRef::new(&middle_id, "in1"),
    );
    graph.traversal_mut().add_e(
        OutputLinkPortRef::new(&middle_id, "out1"),
        InputLinkPortRef::new(&downstream_id, "in1"),
    );

    assert_eq!(graph.traversal().v(()).ids().len(), 3);
    assert_eq!(graph.traversal().e(()).ids().len(), 2);
}
