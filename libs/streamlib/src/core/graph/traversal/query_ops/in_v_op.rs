// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{graph::ProcessorTraversal, LinkTraversal};

impl<'a> LinkTraversal<'a> {
    /// Get the outgoing vertex which should be the source vertex of the edge.
    pub fn in_v(self) -> ProcessorTraversal<'a> {
        let mut outgoing_node_ids = Vec::new();
        for edge_id in self.ids {
            match self.graph.edge_endpoints(edge_id) {
                Some((_, dst)) => outgoing_node_ids.push(dst),
                None => continue,
            }
        }

        ProcessorTraversal {
            graph: self.graph,
            ids: outgoing_node_ids,
        }
    }
}
