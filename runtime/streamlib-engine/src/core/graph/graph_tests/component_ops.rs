// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_insert_and_get_component() {
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
        .insert(MockState(ProcessorState::Running));

    let state = graph
        .traversal()
        .v(id.as_str())
        .first()
        .unwrap()
        .get::<MockState>();

    assert!(state.is_some());
    assert_eq!(state.unwrap().0, ProcessorState::Running);
}

#[test]
fn test_has_component() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();

    assert!(
        !graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .has::<MockState>()
    );

    graph
        .traversal_mut()
        .v(id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockState::default());

    assert!(
        graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .has::<MockState>()
    );
}

#[test]
fn test_remove_component() {
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
        .insert(MockState(ProcessorState::Running));

    let removed = graph
        .traversal_mut()
        .v(id.as_str())
        .first_mut()
        .unwrap()
        .remove::<MockState>();

    assert!(removed.is_some());
    assert_eq!(removed.unwrap().0, ProcessorState::Running);
    assert!(
        !graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .has::<MockState>()
    );
}

#[test]
fn test_multiple_components_on_same_node() {
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
        node.insert(MockState(ProcessorState::Running));
        node.insert(MockProcessorInstance);
        node.insert(CounterComponent(42));
    }

    let node = graph.traversal().v(id.as_str()).first().unwrap();
    assert!(node.has::<MockState>());
    assert!(node.has::<MockProcessorInstance>());
    assert!(node.has::<CounterComponent>());
    assert_eq!(node.get::<CounterComponent>().unwrap().0, 42);
}

#[test]
fn test_link_components() {
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

    graph
        .traversal_mut()
        .e(link_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockLinkInstance::new(8));

    let instance = graph
        .traversal()
        .e(link_id.as_str())
        .first()
        .unwrap()
        .get::<MockLinkInstance>();

    assert!(instance.is_some());
    assert_eq!(instance.unwrap().capacity, 8);
}
