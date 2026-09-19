// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::{Direction, visit::EdgeRef};

use super::super::traversal_source::LinkLocation;

/// Every link carrying out of `node_ids` — the digraph's outgoing edges, and
/// nothing else.
///
/// A link from another runtime is never among them: no node here produces one,
/// so a caller walking a source port's own links is right to see none.
fn every_link_carrying_out_of(
    graph: &DiGraph<ProcessorNode, Link>,
    node_ids: Vec<NodeIndex>,
) -> Vec<LinkLocation> {
    let mut carrying_out = Vec::new();
    for node_idx in node_ids {
        for edge in graph.edges_directed(node_idx, Direction::Outgoing) {
            carrying_out.push(LinkLocation::OnAnEdgeOfTheDigraph(edge.id()));
        }
    }
    carrying_out
}

impl<'a> ProcessorTraversal<'a> {
    /// Get the outgoing edges.
    pub fn out_e(self) -> LinkTraversal<'a> {
        let ids = every_link_carrying_out_of(self.graph, self.ids);
        LinkTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    /// Get the outgoing edges.
    pub fn out_e(self) -> LinkTraversalMut<'a> {
        let ids = every_link_carrying_out_of(self.graph, self.ids);
        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}
