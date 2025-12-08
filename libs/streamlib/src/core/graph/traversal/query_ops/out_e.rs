// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{graph::ProcessorTraversal, LinkTraversal};
use petgraph::{visit::EdgeRef, Direction};

impl<'a> ProcessorTraversal<'a> {
    /// Get the first vertex in the current traversal.
    pub fn out_e(self) -> LinkTraversal<'a> {
        let mut downstream_ids = Vec::new();

        for node_idx in self.ids {
            for edge in self.graph.edges_directed(node_idx, Direction::Outgoing) {
                downstream_ids.push(edge.id());
            }
        }

        LinkTraversal {
            graph: self.graph,
            ids: downstream_ids,
        }
    }
}
