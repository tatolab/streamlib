// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{
    GraphNode, Link, LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut,
};
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};
use petgraph::{Direction, visit::EdgeRef};

/// Every link carrying into `node_ids` — the digraph's incoming edges.
fn every_link_carrying_into(
    graph: &DiGraph<GraphNode, Link>,
    node_ids: &[NodeIndex],
) -> Vec<EdgeIndex> {
    node_ids
        .iter()
        .flat_map(|&node_idx| graph.edges_directed(node_idx, Direction::Incoming))
        .map(|edge| edge.id())
        .collect()
}

impl<'a> NodeTraversal<'a> {
    /// Get the incoming edges
    pub fn in_e(self) -> LinkTraversal<'a> {
        let ids = every_link_carrying_into(self.graph, &self.ids);
        LinkTraversal {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> NodeTraversalMut<'a> {
    /// Get the incoming edges
    pub fn in_e(self) -> LinkTraversalMut<'a> {
        let ids = every_link_carrying_into(self.graph, &self.ids);
        LinkTraversalMut {
            graph: self.graph,
            ids,
        }
    }
}
