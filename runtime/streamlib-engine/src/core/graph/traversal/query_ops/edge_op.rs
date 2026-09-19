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

use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, LinkUniqueId, LinksFromAnotherRuntime, ProcessorNode,
    TraversalSource, TraversalSourceMut,
};

use super::super::traversal_source::LinkLocation;

/// Where the link with this id lives, or nothing when the graph holds none.
fn where_this_link_lives(
    graph: &DiGraph<ProcessorNode, Link>,
    links_from_another_runtime: &LinksFromAnotherRuntime,
    link_id: &LinkUniqueId,
) -> Vec<LinkLocation> {
    if let Some(edge) = graph
        .edge_references()
        .find(|edge_ref| &edge_ref.weight().id == link_id)
    {
        return vec![LinkLocation::OnAnEdgeOfTheDigraph(edge.id())];
    }
    links_from_another_runtime
        .get(link_id)
        .map(|link| {
            vec![LinkLocation::AmongTheLinksFromAnotherRuntime(
                link.id.clone(),
            )]
        })
        .unwrap_or_default()
}

/// Every link in the graph — the digraph's edges first, then the links from
/// another runtime, each group in the order it was connected.
fn every_link(
    graph: &DiGraph<ProcessorNode, Link>,
    links_from_another_runtime: &LinksFromAnotherRuntime,
) -> Vec<LinkLocation> {
    graph
        .edge_references()
        .map(|edge_ref| LinkLocation::OnAnEdgeOfTheDigraph(edge_ref.id()))
        .chain(
            links_from_another_runtime
                .every_link()
                .map(|link| LinkLocation::AmongTheLinksFromAnotherRuntime(link.id.clone())),
        )
        .collect()
}

impl<'a> TraversalSource<'a> {
    /// Start traversal from edges.
    ///
    /// Accepts:
    /// - `()` - all edges
    /// - `&str` - edge by ID string
    /// - `LinkUniqueId` - edge by ID
    pub fn e(self, filter: impl private::IntoEdgeFilter) -> LinkTraversal<'a> {
        let ids = match filter.into_filter() {
            Some(link_id) => {
                where_this_link_lives(self.graph, self.links_from_another_runtime, &link_id)
            }
            None => every_link(self.graph, self.links_from_another_runtime),
        };
        LinkTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
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
        let ids = match filter.into_filter() {
            Some(link_id) => {
                where_this_link_lives(self.graph, self.links_from_another_runtime, &link_id)
            }
            None => every_link(self.graph, self.links_from_another_runtime),
        };
        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}
