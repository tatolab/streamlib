// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Query builder types for graph operations.

use crate::core::graph::{Link, ProcessorNode, ProcessorUniqueId};

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

/// Entry point for graph queries.
pub struct TraversalSource<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<ProcessorNode, Link>,
}

impl<'a> TraversalSource<'a> {
    /// Create a new query builder for the given graph.
    pub(in crate::core::graph) fn new(graph: &'a DiGraph<ProcessorNode, Link>) -> Self {
        Self { graph }
    }
}

/// Read-only query over processor nodes.
pub struct ProcessorTraversal<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<NodeIndex>,
}

/// Read-only query over links.
pub struct LinkTraversal<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<EdgeIndex>,
}

// =============================================================================
// Mutable Traversal Types
// =============================================================================

/// Entry point for mutable graph traversals.
pub struct TraversalSourceMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<ProcessorNode, Link>,
}

impl<'a> TraversalSourceMut<'a> {
    /// Create a new mutable traversal source for the given graph.
    pub(in crate::core::graph) fn new(graph: &'a mut DiGraph<ProcessorNode, Link>) -> Self {
        Self { graph }
    }

    /// A link traversal over nothing — what an op that could not add its link
    /// hands back.
    pub(in crate::core::graph::traversal) fn no_link(self) -> LinkTraversalMut<'a> {
        LinkTraversalMut {
            graph: self.graph,
            ids: vec![],
        }
    }
}

/// The node `processor_id` names, if the digraph holds one.
pub(in crate::core::graph::traversal) fn node_index_of(
    graph: &DiGraph<ProcessorNode, Link>,
    processor_id: &ProcessorUniqueId,
) -> Option<NodeIndex> {
    graph
        .node_indices()
        .find(|&node_idx| &graph[node_idx].id == processor_id)
}

/// Mutable query over processor nodes.
pub struct ProcessorTraversalMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<NodeIndex>,
}

/// Mutable query over links.
pub struct LinkTraversalMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<EdgeIndex>,
}
