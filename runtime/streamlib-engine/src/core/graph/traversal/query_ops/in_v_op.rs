// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{
    GraphNode, Link, LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut,
};
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

/// The node each of these links carries into.
fn every_destination_node_of(
    graph: &DiGraph<GraphNode, Link>,
    links: &[EdgeIndex],
) -> Vec<NodeIndex> {
    links
        .iter()
        .filter_map(|&edge| graph.edge_endpoints(edge))
        .map(|(_, destination)| destination)
        .collect()
}

impl<'a> LinkTraversal<'a> {
    /// Get the vertex each link carries into — its destination.
    pub fn in_v(self) -> NodeTraversal<'a> {
        let ids = every_destination_node_of(self.graph, &self.ids);
        NodeTraversal {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Get the vertex each link carries into — its destination.
    pub fn in_v(self) -> NodeTraversalMut<'a> {
        let ids = every_destination_node_of(self.graph, &self.ids);
        NodeTraversalMut {
            graph: self.graph,
            ids,
        }
    }
}
