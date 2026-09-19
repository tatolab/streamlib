// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Component, GraphEdgeWithComponents, GraphNodeWithComponents, LinkTraversal, LinkTraversalMut,
    ProcessorTraversal, ProcessorTraversalMut,
};

use super::super::traversal_source::link_at;

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
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}

impl<'a> LinkTraversal<'a> {
    /// Filter to links that have the specified component.
    pub fn has_component<C: Component>(self) -> Self {
        let ids = self
            .ids
            .iter()
            .filter(|at| {
                link_at(self.graph, self.links_from_another_runtime, at)
                    .is_some_and(|link| link.has::<C>())
            })
            .cloned()
            .collect();

        Self {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
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
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Filter to links that have the specified component.
    pub fn has_component<C: Component>(self) -> Self {
        let ids = self
            .ids
            .iter()
            .filter(|at| {
                link_at(self.graph, self.links_from_another_runtime, at)
                    .is_some_and(|link| link.has::<C>())
            })
            .cloned()
            .collect();

        Self {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}
