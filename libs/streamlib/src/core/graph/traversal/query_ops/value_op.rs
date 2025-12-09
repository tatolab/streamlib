// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{Link, LinkTraversal, ProcessorNode, ProcessorTraversal};

impl<'a> ProcessorTraversal<'a> {
    /// Get the first value in the current traversal.
    /// If the traversal is empty, returns None.
    /// If the ids list is more than one element, returns the first element. Please use next or collect to get all elements.
    pub fn value(self) -> Option<&'a ProcessorNode> {
        self.ids.first().and_then(|id| self.graph.node_weight(*id))
    }
}

impl<'a> LinkTraversal<'a> {
    /// Get the first value in the current traversal.
    /// If the traversal is empty, returns None.
    /// If the ids list is more than one element, returns the first element. Please use next or collect to get all elements.
    pub fn value(self) -> Option<&'a Link> {
        self.ids.first().and_then(|id| self.graph.edge_weight(*id))
    }
}
