// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{Link, LinkTraversalMut, ProcessorNode, ProcessorTraversalMut};

impl<'a> ProcessorTraversalMut<'a> {
    pub fn first_mut(self) -> Option<&'a mut ProcessorNode> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.node_weight_mut(idx))
    }

    pub fn last_mut(self) -> Option<&'a mut ProcessorNode> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.node_weight_mut(idx))
    }

    pub fn nth_mut(self, n: usize) -> Option<&'a mut ProcessorNode> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.node_weight_mut(idx))
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn first_mut(self) -> Option<&'a mut Link> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.edge_weight_mut(idx))
    }

    pub fn last_mut(self) -> Option<&'a mut Link> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.edge_weight_mut(idx))
    }

    pub fn nth_mut(self, n: usize) -> Option<&'a mut Link> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.edge_weight_mut(idx))
    }
}
