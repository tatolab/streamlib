// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::{Direction, visit::EdgeRef};

use super::super::traversal_source::LinkLocation;

impl<'a> ProcessorTraversal<'a> {
    /// Get the outgoing edges.
    ///
    /// Never yields a link from another runtime: no node here produces one, so
    /// a caller walking a source port's own links is right to see none.
    pub fn out_e(self) -> LinkTraversal<'a> {
        let mut outgoing_edge_ids = Vec::new();

        for node_idx in self.ids {
            for edge in self.graph.edges_directed(node_idx, Direction::Outgoing) {
                outgoing_edge_ids.push(LinkLocation::OnAnEdgeOfTheDigraph(edge.id()));
            }
        }

        LinkTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: outgoing_edge_ids,
        }
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    /// Get the outgoing edges.
    ///
    /// Never yields a link from another runtime: no node here produces one, so
    /// a caller walking a source port's own links is right to see none.
    pub fn out_e(self) -> LinkTraversalMut<'a> {
        let mut outgoing_edge_ids = Vec::new();

        for node_idx in self.ids {
            for edge in self.graph.edges_directed(node_idx, Direction::Outgoing) {
                outgoing_edge_ids.push(LinkLocation::OnAnEdgeOfTheDigraph(edge.id()));
            }
        }

        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: outgoing_edge_ids,
        }
    }
}
