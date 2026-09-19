// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::{Direction, visit::EdgeRef};

use super::super::traversal_source::LinkLocation;
use crate::core::graph::{Link, LinksFromAnotherRuntime, ProcessorNode};

/// Every link carrying into `node_ids` — the digraph's incoming edges, and the
/// links from another runtime whose destination is one of those nodes.
///
/// The second half is what keeps a remote link counted everywhere a
/// destination's inbound links are: the fan-in cap, the windowed-port refusal,
/// the notify service's sizing.
fn every_link_carrying_into(
    graph: &DiGraph<ProcessorNode, Link>,
    links_from_another_runtime: &LinksFromAnotherRuntime,
    node_ids: Vec<NodeIndex>,
) -> Vec<LinkLocation> {
    let mut carrying_in = Vec::new();
    for node_idx in node_ids {
        for edge in graph.edges_directed(node_idx, Direction::Incoming) {
            carrying_in.push(LinkLocation::OnAnEdgeOfTheDigraph(edge.id()));
        }
        let Some(node) = graph.node_weight(node_idx) else {
            continue;
        };
        for link in links_from_another_runtime.every_link_into(&node.id) {
            carrying_in.push(LinkLocation::AmongTheLinksFromAnotherRuntime(
                link.id.clone(),
            ));
        }
    }
    carrying_in
}

impl<'a> ProcessorTraversal<'a> {
    /// Get the incoming edges
    pub fn in_e(self) -> LinkTraversal<'a> {
        let ids = every_link_carrying_into(self.graph, self.links_from_another_runtime, self.ids);
        LinkTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    /// Get the incoming edges
    pub fn in_e(self) -> LinkTraversalMut<'a> {
        let ids = every_link_carrying_into(self.graph, self.links_from_another_runtime, self.ids);
        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}
