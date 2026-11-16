// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{
    GraphNode, Link, LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut,
};

impl<'a> NodeTraversal<'a> {
    pub fn iter(self) -> impl Iterator<Item = &'a GraphNode> {
        self.into_iter()
    }
}

impl<'a> IntoIterator for NodeTraversal<'a> {
    type Item = &'a GraphNode;
    type IntoIter = std::vec::IntoIter<&'a GraphNode>;

    fn into_iter(self) -> Self::IntoIter {
        let NodeTraversal { graph, ids } = self;
        ids.into_iter()
            .filter_map(|idx| graph.node_weight(idx))
            .collect::<Vec<&GraphNode>>()
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

impl<'a> NodeTraversalMut<'a> {
    pub fn iter(self) -> impl Iterator<Item = &'a GraphNode> {
        self.into_iter()
    }
}

impl<'a> IntoIterator for NodeTraversalMut<'a> {
    type Item = &'a GraphNode;
    type IntoIter = std::vec::IntoIter<&'a GraphNode>;

    fn into_iter(self) -> Self::IntoIter {
        let NodeTraversalMut { graph, ids } = self;
        ids.into_iter()
            .filter_map(|idx| graph.node_weight(idx))
            .collect::<Vec<&GraphNode>>()
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
