// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Query builder types for graph operations.

use crate::core::graph::{Link, ProcessorNode};

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

/// Entry point for graph queries.
pub struct TraversalSource<'a> {
    pub(in crate::core::graph::query) graph: &'a DiGraph<ProcessorNode, Link>,
}

impl<'a> TraversalSource<'a> {
    /// Create a new query builder for the given graph.
    pub(in crate::core::graph) fn new(graph: &'a DiGraph<ProcessorNode, Link>) -> Self {
        Self { graph }
    }
}

/// Read-only query over processor nodes.
pub struct ProcessorTraversal<'a> {
    pub(in crate::core::graph::query) graph: &'a DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::query) ids: Vec<NodeIndex>,
}

/// Read-only query over links.
pub struct LinkTraversal<'a> {
    pub(in crate::core::graph::query) graph: &'a DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::query) ids: Vec<EdgeIndex>,
}
