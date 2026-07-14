// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    InputLinkPortRef, Link, LinkTraversalMut, OutputLinkPortRef, TraversalSourceMut,
};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new edge (link) between two ports.
    ///
    /// Type-safe: `from` must be an output port, `to` must be an input port.
    pub fn add_e(self, from: OutputLinkPortRef, to: InputLinkPortRef) -> LinkTraversalMut<'a> {
        // 1. Find source and target node indices
        let Some(from_idx) = self
            .graph
            .node_indices()
            .find(|&idx| self.graph[idx].id.as_str() == from.processor_id.as_str())
        else {
            return LinkTraversalMut {
                graph: self.graph,
                ids: vec![],
            };
        };

        let Some(to_idx) = self
            .graph
            .node_indices()
            .find(|&idx| self.graph[idx].id.as_str() == to.processor_id.as_str())
        else {
            return LinkTraversalMut {
                graph: self.graph,
                ids: vec![],
            };
        };

        // 2. Validate ports exist on the processors
        let from_node = &self.graph[from_idx];
        if !from_node.has_output(&from.port_name) {
            return LinkTraversalMut {
                graph: self.graph,
                ids: vec![],
            };
        }

        let to_node = &self.graph[to_idx];
        if !to_node.has_input(&to.port_name) {
            return LinkTraversalMut {
                graph: self.graph,
                ids: vec![],
            };
        }

        // 3. Create link and add edge
        let link = Link::new(&format!("{}", from), &format!("{}", to));
        let edge_idx = self.graph.add_edge(from_idx, to_idx, link);

        // 4. Return traversal with new edge
        LinkTraversalMut {
            graph: self.graph,
            ids: vec![edge_idx],
        }
    }
}
