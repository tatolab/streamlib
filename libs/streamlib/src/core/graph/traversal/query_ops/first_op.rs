// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut};

impl<'a> ProcessorTraversal<'a> {
    pub fn first(self) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.node_weight(idx))
    }
}

impl<'a> LinkTraversal<'a> {
    pub fn first(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.edge_weight(idx))
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    pub fn first(self) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.node_weight(idx))
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn first(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.edge_weight(idx))
    }
}
