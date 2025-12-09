// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{graph::ProcessorTraversal, LinkTraversal};
use petgraph::{visit::EdgeRef, Direction};

impl<'a> ProcessorTraversal<'a> {
    /// Get the incoming edges
    pub fn in_e(self) -> LinkTraversal<'a> {
        let mut incoming_edge_ids = Vec::new();

        for node_idx in self.ids {
            for edge in self.graph.edges_directed(node_idx, Direction::Incoming) {
                incoming_edge_ids.push(edge.id());
            }
        }

        LinkTraversal {
            graph: self.graph,
            ids: incoming_edge_ids,
        }
    }
}
