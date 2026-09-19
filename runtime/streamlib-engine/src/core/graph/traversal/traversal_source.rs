// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Query builder types for graph operations.

use crate::core::graph::{
    Link, LinkUniqueId, LinksFromAnotherRuntime, ProcessorNode, ProcessorUniqueId,
};

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

/// Where one link the traversal is carrying lives.
///
/// A link whose source is a port on this runtime is an edge of the digraph; one
/// whose source is on another runtime has no node here to hang an edge on and
/// is kept beside it. Both are links, and every op reads one the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core::graph::traversal) enum LinkLocation {
    OnAnEdgeOfTheDigraph(EdgeIndex),
    AmongTheLinksFromAnotherRuntime(LinkUniqueId),
}

/// The link at `location`, wherever it lives.
pub(in crate::core::graph::traversal) fn link_at<'a>(
    graph: &'a DiGraph<ProcessorNode, Link>,
    links_from_another_runtime: &'a LinksFromAnotherRuntime,
    location: &LinkLocation,
) -> Option<&'a Link> {
    match location {
        LinkLocation::OnAnEdgeOfTheDigraph(edge) => graph.edge_weight(*edge),
        LinkLocation::AmongTheLinksFromAnotherRuntime(link_id) => {
            links_from_another_runtime.get(link_id)
        }
    }
}

/// The link at `location`, to be changed.
pub(in crate::core::graph::traversal) fn link_at_mut<'a>(
    graph: &'a mut DiGraph<ProcessorNode, Link>,
    links_from_another_runtime: &'a mut LinksFromAnotherRuntime,
    location: &LinkLocation,
) -> Option<&'a mut Link> {
    match location {
        LinkLocation::OnAnEdgeOfTheDigraph(edge) => graph.edge_weight_mut(*edge),
        LinkLocation::AmongTheLinksFromAnotherRuntime(link_id) => {
            links_from_another_runtime.get_mut(link_id)
        }
    }
}

/// Entry point for graph queries.
pub struct TraversalSource<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) links_from_another_runtime: &'a LinksFromAnotherRuntime,
}

impl<'a> TraversalSource<'a> {
    /// Create a new query builder for the given graph.
    pub(in crate::core::graph) fn new(
        graph: &'a DiGraph<ProcessorNode, Link>,
        links_from_another_runtime: &'a LinksFromAnotherRuntime,
    ) -> Self {
        Self {
            graph,
            links_from_another_runtime,
        }
    }
}

/// Read-only query over processor nodes.
pub struct ProcessorTraversal<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) links_from_another_runtime: &'a LinksFromAnotherRuntime,
    pub(in crate::core::graph::traversal) ids: Vec<NodeIndex>,
}

/// Read-only query over links.
pub struct LinkTraversal<'a> {
    pub(in crate::core::graph::traversal) graph: &'a DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) links_from_another_runtime: &'a LinksFromAnotherRuntime,
    pub(in crate::core::graph::traversal) ids: Vec<LinkLocation>,
}

// =============================================================================
// Mutable Traversal Types
// =============================================================================

/// Entry point for mutable graph traversals.
pub struct TraversalSourceMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) links_from_another_runtime:
        &'a mut LinksFromAnotherRuntime,
}

impl<'a> TraversalSourceMut<'a> {
    /// Create a new mutable traversal source for the given graph.
    pub(in crate::core::graph) fn new(
        graph: &'a mut DiGraph<ProcessorNode, Link>,
        links_from_another_runtime: &'a mut LinksFromAnotherRuntime,
    ) -> Self {
        Self {
            graph,
            links_from_another_runtime,
        }
    }

    /// A link traversal over nothing — what an op that could not add its link
    /// hands back.
    pub(in crate::core::graph::traversal) fn no_link(self) -> LinkTraversalMut<'a> {
        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
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
    pub(in crate::core::graph::traversal) links_from_another_runtime:
        &'a mut LinksFromAnotherRuntime,
    pub(in crate::core::graph::traversal) ids: Vec<NodeIndex>,
}

/// Mutable query over links.
pub struct LinkTraversalMut<'a> {
    pub(in crate::core::graph::traversal) graph: &'a mut DiGraph<ProcessorNode, Link>,
    pub(in crate::core::graph::traversal) links_from_another_runtime:
        &'a mut LinksFromAnotherRuntime,
    pub(in crate::core::graph::traversal) ids: Vec<LinkLocation>,
}
