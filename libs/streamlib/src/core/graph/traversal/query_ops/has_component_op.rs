// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Component, GraphEdge, GraphNode, LinkTraversal, LinkTraversalMut, ProcessorTraversal,
    ProcessorTraversalMut,
};

impl<'a> ProcessorTraversal<'a> {
    /// Filter to nodes that have the specified component.
    pub fn has_component<C: Component>(self) -> Self {
        let ids = self
            .ids
            .into_iter()
            .filter(|&idx| {
                self.graph
                    .node_weight(idx)
                    .map(|node| node.has::<C>())
                    .unwrap_or(false)
            })
            .collect();

        Self {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> LinkTraversal<'a> {
    /// Filter to links that have the specified component.
    pub fn has_component<C: Component>(self) -> Self {
        let ids = self
            .ids
            .into_iter()
            .filter(|&idx| {
                self.graph
                    .edge_weight(idx)
                    .map(|link| link.has::<C>())
                    .unwrap_or(false)
            })
            .collect();

        Self {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    /// Filter to nodes that have the specified component.
    pub fn has_component<C: Component>(self) -> Self {
        let ids = self
            .ids
            .into_iter()
            .filter(|&idx| {
                self.graph
                    .node_weight(idx)
                    .map(|node| node.has::<C>())
                    .unwrap_or(false)
            })
            .collect();

        Self {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Filter to links that have the specified component.
    pub fn has_component<C: Component>(self) -> Self {
        let ids = self
            .ids
            .into_iter()
            .filter(|&idx| {
                self.graph
                    .edge_weight(idx)
                    .map(|link| link.has::<C>())
                    .unwrap_or(false)
            })
            .collect();

        Self {
            graph: self.graph,
            ids,
        }
    }
}
