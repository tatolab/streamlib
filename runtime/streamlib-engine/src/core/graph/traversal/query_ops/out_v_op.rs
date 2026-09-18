// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::NodeIndex;

use super::super::traversal_source::LinkLocation;

/// The node each of these links carries from.
///
/// A link from another runtime carries from no node here, so it contributes
/// none — which is the honest answer, not an omission.
fn every_source_node_of(
    graph: &petgraph::graph::DiGraph<crate::core::graph::ProcessorNode, crate::core::graph::Link>,
    links: &[LinkLocation],
) -> Vec<NodeIndex> {
    links
        .iter()
        .filter_map(|at| match at {
            LinkLocation::OnAnEdgeOfTheDigraph(edge) => {
                graph.edge_endpoints(*edge).map(|(source, _)| source)
            }
            LinkLocation::AmongTheLinksFromAnotherRuntime(_) => None,
        })
        .collect()
}

impl<'a> LinkTraversal<'a> {
    /// Get the vertex each link carries from — its source.
    pub fn out_v(self) -> ProcessorTraversal<'a> {
        let ids = every_source_node_of(self.graph, &self.ids);
        ProcessorTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Get the vertex each link carries from — its source.
    pub fn out_v(self) -> ProcessorTraversalMut<'a> {
        let ids = every_source_node_of(self.graph, &self.ids);
        ProcessorTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids,
        }
    }
}
