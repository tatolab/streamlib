// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};
use petgraph::{Direction, visit::EdgeRef};

/// Every link carrying out of `node_ids` — the digraph's outgoing edges.
fn every_link_carrying_out_of(
    graph: &DiGraph<ProcessorNode, Link>,
    node_ids: &[NodeIndex],
) -> Vec<EdgeIndex> {
    node_ids
        .iter()
        .flat_map(|&node_idx| graph.edges_directed(node_idx, Direction::Outgoing))
        .map(|edge| edge.id())
        .collect()
}

impl<'a> ProcessorTraversal<'a> {
    /// Get the outgoing edges.
    pub fn out_e(self) -> LinkTraversal<'a> {
        let ids = every_link_carrying_out_of(self.graph, &self.ids);
        LinkTraversal {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    /// Get the outgoing edges.
    pub fn out_e(self) -> LinkTraversalMut<'a> {
        let ids = every_link_carrying_out_of(self.graph, &self.ids);
        LinkTraversalMut {
            graph: self.graph,
            ids,
        }
    }
}
