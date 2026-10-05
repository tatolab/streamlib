// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};
use petgraph::{Direction, visit::EdgeRef};

/// Every link carrying into `node_ids` — the digraph's incoming edges.
fn every_link_carrying_into(
    graph: &DiGraph<ProcessorNode, Link>,
    node_ids: Vec<NodeIndex>,
) -> Vec<EdgeIndex> {
    node_ids
        .into_iter()
        .flat_map(|node_idx| graph.edges_directed(node_idx, Direction::Incoming))
        .map(|edge| edge.id())
        .collect()
}

impl<'a> ProcessorTraversal<'a> {
    /// Get the incoming edges
    pub fn in_e(self) -> LinkTraversal<'a> {
        let ids = every_link_carrying_into(self.graph, self.ids);
        LinkTraversal {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    /// Get the incoming edges
    pub fn in_e(self) -> LinkTraversalMut<'a> {
        let ids = every_link_carrying_into(self.graph, self.ids);
        LinkTraversalMut {
            graph: self.graph,
            ids,
        }
    }
}
