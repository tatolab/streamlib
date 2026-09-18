// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};

use super::super::traversal_source::{link_at, link_at_mut};

impl<'a> ProcessorTraversal<'a> {
    pub fn nth(self, n: usize) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.node_weight(idx))
    }
}

impl<'a> LinkTraversal<'a> {
    pub fn nth(self, n: usize) -> Option<&'a Link> {
        let at = self.ids.into_iter().nth(n)?;
        link_at(self.graph, self.links_from_another_runtime, &at)
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    pub fn nth(self, n: usize) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn nth_mut(self, n: usize) -> Option<&'a mut ProcessorNode> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.node_weight_mut(idx))
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn nth(self, n: usize) -> Option<&'a Link> {
        let at = self.ids.into_iter().nth(n)?;
        link_at(self.graph, self.links_from_another_runtime, &at)
    }

    pub fn nth_mut(self, n: usize) -> Option<&'a mut Link> {
        let at = self.ids.into_iter().nth(n)?;
        link_at_mut(self.graph, self.links_from_another_runtime, &at)
    }
}
