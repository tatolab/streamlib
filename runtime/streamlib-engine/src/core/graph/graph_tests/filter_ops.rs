// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Filtering Operations.

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_filter_by_processor_type() {
    let mut graph = test_graph();

    graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named");
    graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named");
    graph
        .traversal_mut()
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named");

    let mock_processors: Vec<_> = graph
        .traversal()
        .v(())
        .filter(|n| n.processor_type == MockProcessor::Processor::processor_class_import_path())
        .ids();

    assert_eq!(mock_processors.len(), 2);
}

#[test]
fn test_has_component_filters_correctly() {
    let mut graph = test_graph();

    let id1 = graph
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
        .insert(MockState::default());

    let with_state: Vec<_> = graph.traversal().v(()).has_component::<MockState>().ids();

    assert_eq!(with_state.len(), 1);
    assert_eq!(with_state[0].as_str(), id1);
}

#[test]
fn test_filter_links_by_destination() {
    let mut graph = test_graph();

    let upstream_id = graph
        .traversal_mut()
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let downstream1_id = graph
        .traversal_mut()
        .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let downstream2_id = graph
        .traversal_mut()
        .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();

    graph.traversal_mut().add_e(
        OutputLinkPortRef::new(&upstream_id, "out1"),
        InputLinkPortRef::new(&downstream1_id, "in1"),
    );
    graph.traversal_mut().add_e(
        OutputLinkPortRef::new(&upstream_id, "out2"),
        InputLinkPortRef::new(&downstream2_id, "in1"),
    );

    let to_downstream1: Vec<_> = graph
        .traversal()
        .e(())
        .filter(|link| link.to_port().processor_id().as_str() == downstream1_id)
        .ids();

    assert_eq!(to_downstream1.len(), 1);
}
