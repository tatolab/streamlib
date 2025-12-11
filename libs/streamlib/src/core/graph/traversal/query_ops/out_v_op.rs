// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut};

impl<'a> LinkTraversal<'a> {
    /// Get the outgoing vertex which should be the source vertex of the edge.
    pub fn out_v(self) -> ProcessorTraversal<'a> {
        let mut outgoing_node_ids = Vec::new();
        for edge_id in self.ids {
            match self.graph.edge_endpoints(edge_id) {
                Some((src, _)) => outgoing_node_ids.push(src),
                None => continue,
            }
        }

        ProcessorTraversal {
            graph: self.graph,
            ids: outgoing_node_ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Get the outgoing vertex which should be the source vertex of the edge.
    pub fn out_v(self) -> ProcessorTraversalMut<'a> {
        let mut outgoing_node_ids = Vec::new();
        for edge_id in self.ids {
            match self.graph.edge_endpoints(edge_id) {
                Some((src, _)) => outgoing_node_ids.push(src),
                None => continue,
            }
        }

        ProcessorTraversalMut {
            graph: self.graph,
            ids: outgoing_node_ids,
        }
    }
}
