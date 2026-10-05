// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod private {
    use crate::core::graph::LinkUniqueId;

    pub trait IntoEdgeFilter {
        fn into_filter(self) -> Option<LinkUniqueId>;
    }

    impl IntoEdgeFilter for () {
        fn into_filter(self) -> Option<LinkUniqueId> {
            None
        }
    }

    impl IntoEdgeFilter for LinkUniqueId {
        fn into_filter(self) -> Option<LinkUniqueId> {
            Some(self)
        }
    }

    impl IntoEdgeFilter for &LinkUniqueId {
        fn into_filter(self) -> Option<LinkUniqueId> {
            Some(self.clone())
        }
    }

    impl IntoEdgeFilter for &str {
        fn into_filter(self) -> Option<LinkUniqueId> {
            Some(LinkUniqueId::from(self))
        }
    }
}

use petgraph::graph::{DiGraph, EdgeIndex};
use petgraph::visit::EdgeRef;

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, LinkUniqueId, ProcessorNode, TraversalSource,
    TraversalSourceMut,
};

/// The edge of the link with this id, or every edge when no id is given.
fn every_link_edge_the_filter_selects(
    graph: &DiGraph<ProcessorNode, Link>,
    link_id_filter: Option<LinkUniqueId>,
) -> Vec<EdgeIndex> {
    match link_id_filter {
        Some(link_id) => graph
            .edge_references()
            .find(|edge_ref| edge_ref.weight().id == link_id)
            .map(|edge_ref| vec![edge_ref.id()])
            .unwrap_or_default(),
        None => graph
            .edge_references()
            .map(|edge_ref| edge_ref.id())
            .collect(),
    }
}

impl<'a> TraversalSource<'a> {
    /// Start traversal from edges.
    ///
    /// Accepts:
    /// - `()` - all edges
    /// - `&str` - edge by ID string
    /// - `LinkUniqueId` - edge by ID
    pub fn e(self, filter: impl private::IntoEdgeFilter) -> LinkTraversal<'a> {
        let ids = every_link_edge_the_filter_selects(self.graph, filter.into_filter());
        LinkTraversal {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> TraversalSourceMut<'a> {
    /// Start traversal from edges.
    ///
    /// Accepts:
    /// - `()` - all edges
    /// - `&str` - edge by ID string
    /// - `LinkUniqueId` - edge by ID
    pub fn e(self, filter: impl private::IntoEdgeFilter) -> LinkTraversalMut<'a> {
        let ids = every_link_edge_the_filter_selects(self.graph, filter.into_filter());
        LinkTraversalMut {
            graph: self.graph,
            ids,
        }
    }
}
