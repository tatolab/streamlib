// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_out_e_returns_outgoing_links() {
    let mut graph = test_graph();

    let upstream_id = graph
        .traversal_mut()
        .add_v(MockOutputOnlyNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();
    let downstream1_id = graph
        .traversal_mut()
        .add_v(MockInputOnlyNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();
    let downstream2_id = graph
        .traversal_mut()
        .add_v(MockInputOnlyNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
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

    let outgoing = graph.traversal().v(upstream_id.as_str()).out_e().ids();
    assert_eq!(outgoing.len(), 2);
}

#[test]
fn test_in_e_returns_incoming_links() {
    let mut graph = test_graph();

    let upstream1_id = graph
        .traversal_mut()
        .add_v(MockOutputOnlyNode::Node::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create node")
        .id
        .to_string();
    let upstream2_id = graph
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

    graph.traversal_mut().add_e(
        OutputLinkPortRef::new(&upstream1_id, "out1"),
        InputLinkPortRef::new(&downstream_id, "in1"),
    );
    graph.traversal_mut().add_e(
        OutputLinkPortRef::new(&upstream2_id, "out1"),
        InputLinkPortRef::new(&downstream_id, "in2"),
    );

    let incoming = graph.traversal().v(downstream_id.as_str()).in_e().ids();
    assert_eq!(incoming.len(), 2);
}

#[test]
fn test_out_v_returns_upstream_node() {
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

    // out_v = vertex the edge comes FROM (upstream)
    let upstream_nodes = graph.traversal().e(link_id.as_str()).out_v().ids();
    assert_eq!(upstream_nodes.len(), 1);
    assert_eq!(upstream_nodes[0].as_str(), upstream_id);
}

#[test]
fn test_in_v_returns_downstream_node() {
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

    // in_v = vertex the edge goes INTO (downstream)
    let downstream_nodes = graph.traversal().e(link_id.as_str()).in_v().ids();
    assert_eq!(downstream_nodes.len(), 1);
    assert_eq!(downstream_nodes[0].as_str(), downstream_id);
}
