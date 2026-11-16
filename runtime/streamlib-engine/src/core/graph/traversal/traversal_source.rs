// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Query builder types for graph operations.

use std::collections::HashSet;

use crate::core::graph::{GraphNode, Link, NodeRunId};
use crate::core::nodes::NodeTypesOneStreamResolves;

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

/// Entry point for graph queries.
pub struct TraversalSource<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<GraphNode, Link>,
}

impl<'a> TraversalSource<'a> {
    /// Create a new query builder for the given graph.
    pub(in crate::core::graph) fn new(graph: &'a DiGraph<GraphNode, Link>) -> Self {
        Self { graph }
    }
}

pub struct NodeTraversal<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<GraphNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<NodeIndex>,
}

/// Read-only query over links.
pub struct LinkTraversal<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<GraphNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<EdgeIndex>,
}

// =============================================================================
// Mutable Traversal Types
// =============================================================================

/// Entry point for mutable graph traversals.
pub struct TraversalSourceMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<GraphNode, Link>,
    pub(in crate::core::graph::traversal) node_types_this_stream_resolves:
        &'a NodeTypesOneStreamResolves,
    pub(in crate::core::graph::traversal) node_names_the_graph_only_nodes_hold: &'a HashSet<String>,
}

impl<'a> TraversalSourceMut<'a> {
    /// Create a new mutable traversal source for the given graph.
    pub(in crate::core::graph) fn new(
        graph: &'a mut DiGraph<GraphNode, Link>,
        node_types_this_stream_resolves: &'a NodeTypesOneStreamResolves,
        node_names_the_graph_only_nodes_hold: &'a HashSet<String>,
    ) -> Self {
        Self {
            graph,
            node_types_this_stream_resolves,
            node_names_the_graph_only_nodes_hold,
        }
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

pub(in crate::core::graph::traversal) fn node_index_of(
    graph: &DiGraph<GraphNode, Link>,
    node_run_id: &NodeRunId,
) -> Option<NodeIndex> {
    graph
        .node_indices()
        .find(|&node_idx| &graph[node_idx].id == node_run_id)
}

pub struct NodeTraversalMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<GraphNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<NodeIndex>,
}

/// Mutable query over links.
pub struct LinkTraversalMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<GraphNode, Link>,
    pub(in crate::core::graph::traversal) ids: Vec<EdgeIndex>,
}
