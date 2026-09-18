// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, LinksFromAnotherRuntime, ProcessorNode,
    ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::{DiGraph, NodeIndex};

use super::super::traversal_source::{LinkLocation, link_at};

/// The node each of these links carries into.
///
/// A link from another runtime has a destination node here like any other, so
/// it is resolved by the destination's own processor id rather than by an edge
/// endpoint the digraph does not hold.
fn every_destination_node_of(
    graph: &DiGraph<ProcessorNode, Link>,
    links_from_another_runtime: &LinksFromAnotherRuntime,
    links: &[LinkLocation],
) -> Vec<NodeIndex> {
    let mut destinations = Vec::new();
    for at in links {
        match at {
            LinkLocation::OnAnEdgeOfTheDigraph(edge) => {
                if let Some((_, destination)) = graph.edge_endpoints(*edge) {
                    destinations.push(destination);
                }
            }
            LinkLocation::AmongTheLinksFromAnotherRuntime(_) => {
                let Some(link) = link_at(graph, links_from_another_runtime, at) else {
                    continue;
                };
                let destination_id = &link.to_port().processor_id;
                if let Some(destination) = graph
                    .node_indices()
                    .find(|&idx| &graph[idx].id == destination_id)
                {
                    destinations.push(destination);
                }
            }
        }
    }
    destinations
}

impl<'a> LinkTraversal<'a> {
    /// Get the vertex each link carries into — its destination.
    pub fn in_v(self) -> ProcessorTraversal<'a> {
        let ids = every_destination_node_of(self.graph, self.links_from_another_runtime, &self.ids);
        ProcessorTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Get the vertex each link carries into — its destination.
    pub fn in_v(self) -> ProcessorTraversalMut<'a> {
        let ids = every_destination_node_of(self.graph, self.links_from_another_runtime, &self.ids);
        ProcessorTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}
