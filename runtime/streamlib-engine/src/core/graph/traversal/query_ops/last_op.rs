// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{
    GraphNode, Link, LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut,
};

impl<'a> NodeTraversal<'a> {
    pub fn last(self) -> Option<&'a GraphNode> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.node_weight(idx))
    }
}

impl<'a> LinkTraversal<'a> {
    pub fn last(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.edge_weight(idx))
    }
}

impl<'a> NodeTraversalMut<'a> {
    pub fn last(self) -> Option<&'a GraphNode> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.node_weight(idx))
    }

    pub fn last_mut(self) -> Option<&'a mut GraphNode> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.node_weight_mut(idx))
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn last(self) -> Option<&'a Link> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.edge_weight(idx))
    }

    pub fn last_mut(self) -> Option<&'a mut Link> {
        self.ids
            .into_iter()
            .last()
            .and_then(|idx| self.graph.edge_weight_mut(idx))
    }
}
