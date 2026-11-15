// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Edge (Link) Query Operations.

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_e_all_returns_all_links() {
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

    let link_ids = graph.traversal().e(()).ids();
    assert_eq!(link_ids.len(), 2);
}

#[test]
fn test_e_by_id_returns_specific_link() {
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

    let found = graph.traversal().e(link_id.as_str()).first();
    assert!(found.is_some());
}

#[test]
fn test_link_from_port_and_to_port() {
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

    let link = graph.traversal().e(link_id.as_str()).first().unwrap();

    assert_eq!(link.source().processor_id().as_str(), upstream_id.as_str());
    assert_eq!(link.source().port_name(), "out1");
    assert_eq!(
        link.to_port().processor_id().as_str(),
        downstream_id.as_str()
    );
    assert_eq!(link.to_port().port_name(), "in1");
}
