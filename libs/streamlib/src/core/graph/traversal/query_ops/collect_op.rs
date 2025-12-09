// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{Link, LinkTraversal, ProcessorNode, ProcessorTraversal};

impl<'a> ProcessorTraversal<'a> {
    /// Collects all the nodes in the current traversal.
    /// If the traversal is empty, returns an empty vector.
    pub fn collect(self) -> Vec<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .filter_map(|id| self.graph.node_weight(id))
            .collect()
    }
}

impl<'a> LinkTraversal<'a> {
    /// Collects all the links in the current traversal.
    /// If the traversal is empty, returns an empty vector.
    pub fn collect(self) -> Vec<&'a Link> {
        self.ids
            .into_iter()
            .filter_map(|id| self.graph.edge_weight(id))
            .collect()
    }
}
