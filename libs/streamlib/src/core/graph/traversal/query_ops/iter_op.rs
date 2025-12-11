// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::{EdgeIndex, NodeIndex};

// =============================================================================
// ProcessorTraversal (immutable)
// =============================================================================

impl<'a> ProcessorTraversal<'a> {
    pub fn iter(self) -> impl Iterator<Item = &'a ProcessorNode> {
        self.into_iter()
    }

    pub fn first(self) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn last(self) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn nth(self, n: usize) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn node_ids(self) -> Vec<NodeIndex> {
        self.ids
    }
}

impl<'a> IntoIterator for ProcessorTraversal<'a> {
    type Item = &'a ProcessorNode;
    type IntoIter = std::vec::IntoIter<&'a ProcessorNode>;

    fn into_iter(self) -> Self::IntoIter {
        let ProcessorTraversal { graph, ids } = self;
        ids.into_iter()
            .filter_map(|idx| graph.node_weight(idx))
            .collect::<Vec<&ProcessorNode>>()
            .into_iter()
    }
}

// =============================================================================
// LinkTraversal (immutable)
// =============================================================================

impl<'a> LinkTraversal<'a> {
    pub fn iter(self) -> impl Iterator<Item = &'a Link> {
        self.into_iter()
    }

    pub fn first(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.edge_weight(idx))
    }

    pub fn last(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.edge_weight(idx))
    }

    pub fn nth(self, n: usize) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.edge_weight(idx))
    }

    pub fn edge_ids(self) -> Vec<EdgeIndex> {
        self.ids
    }
}

impl<'a> IntoIterator for LinkTraversal<'a> {
    type Item = &'a Link;
    type IntoIter = std::vec::IntoIter<&'a Link>;

    fn into_iter(self) -> Self::IntoIter {
        let LinkTraversal { graph, ids } = self;
        ids.into_iter()
            .filter_map(|idx| graph.edge_weight(idx))
            .collect::<Vec<&Link>>()
            .into_iter()
    }
}

// =============================================================================
// ProcessorTraversalMut (read-only access via mutable traversal)
// =============================================================================

impl<'a> ProcessorTraversalMut<'a> {
    pub fn iter(self) -> impl Iterator<Item = &'a ProcessorNode> {
        self.into_iter()
    }

    pub fn first(self) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn last(self) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn nth(self, n: usize) -> Option<&'a ProcessorNode> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn node_ids(self) -> Vec<NodeIndex> {
        self.ids
    }
}

impl<'a> IntoIterator for ProcessorTraversalMut<'a> {
    type Item = &'a ProcessorNode;
    type IntoIter = std::vec::IntoIter<&'a ProcessorNode>;

    fn into_iter(self) -> Self::IntoIter {
        let ProcessorTraversalMut { graph, ids } = self;
        ids.into_iter()
            .filter_map(|idx| graph.node_weight(idx))
            .collect::<Vec<&ProcessorNode>>()
            .into_iter()
    }
}

// =============================================================================
// LinkTraversalMut (read-only access via mutable traversal)
// =============================================================================

impl<'a> LinkTraversalMut<'a> {
    pub fn iter(self) -> impl Iterator<Item = &'a Link> {
        self.into_iter()
    }

    pub fn first(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .next()
            .and_then(|idx| self.graph.edge_weight(idx))
    }

    pub fn last(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.edge_weight(idx))
    }

    pub fn nth(self, n: usize) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .nth(n)
            .and_then(|idx| self.graph.edge_weight(idx))
    }

    pub fn edge_ids(self) -> Vec<EdgeIndex> {
        self.ids
    }
}

impl<'a> IntoIterator for LinkTraversalMut<'a> {
    type Item = &'a Link;
    type IntoIter = std::vec::IntoIter<&'a Link>;

    fn into_iter(self) -> Self::IntoIter {
        let LinkTraversalMut { graph, ids } = self;
        ids.into_iter()
            .filter_map(|idx| graph.edge_weight(idx))
            .collect::<Vec<&Link>>()
            .into_iter()
    }
}
