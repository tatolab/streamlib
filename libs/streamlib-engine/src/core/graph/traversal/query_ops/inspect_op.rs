// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};

impl<'a> ProcessorTraversal<'a> {
    /// Calls the provided closure for each processor node in the traversal.
    ///
    /// This is useful for side effects (logging, events, etc.) while continuing
    /// the traversal chain. Similar to [`Iterator::inspect`].
    pub fn inspect<F: FnMut(&ProcessorNode)>(self, mut f: F) -> Self {
        for &idx in &self.ids {
            if let Some(node) = self.graph.node_weight(idx) {
                f(node);
            }
        }
        self
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    /// Calls the provided closure for each processor node in the traversal.
    ///
    /// This is useful for side effects (logging, events, etc.) while continuing
    /// the traversal chain. Similar to [`Iterator::inspect`].
    pub fn inspect<F: FnMut(&ProcessorNode)>(self, mut f: F) -> Self {
        for &idx in &self.ids {
            if let Some(node) = self.graph.node_weight(idx) {
                f(node);
            }
        }
        self
    }
}

impl<'a> LinkTraversal<'a> {
    /// Calls the provided closure for each link in the traversal.
    ///
    /// This is useful for side effects (logging, events, etc.) while continuing
    /// the traversal chain. Similar to [`Iterator::inspect`].
    pub fn inspect<F: FnMut(&Link)>(self, mut f: F) -> Self {
        for &idx in &self.ids {
            if let Some(link) = self.graph.edge_weight(idx) {
                f(link);
            }
        }
        self
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Calls the provided closure for each link in the traversal.
    ///
    /// This is useful for side effects (logging, events, etc.) while continuing
    /// the traversal chain. Similar to [`Iterator::inspect`].
    pub fn inspect<F: FnMut(&Link)>(self, mut f: F) -> Self {
        for &idx in &self.ids {
            if let Some(link) = self.graph.edge_weight(idx) {
                f(link);
            }
        }
        self
    }
}
