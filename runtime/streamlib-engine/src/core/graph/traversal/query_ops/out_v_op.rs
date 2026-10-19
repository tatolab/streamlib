// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

/// The node each of these links carries from.
fn every_source_node_of(
    graph: &DiGraph<ProcessorNode, Link>,
    links: &[EdgeIndex],
) -> Vec<NodeIndex> {
    links
        .iter()
        .filter_map(|&edge| graph.edge_endpoints(edge))
        .map(|(source, _)| source)
        .collect()
}

impl<'a> LinkTraversal<'a> {
    /// Get the vertex each link carries from — its source.
    pub fn out_v(self) -> ProcessorTraversal<'a> {
        let ids = every_source_node_of(self.graph, &self.ids);
        ProcessorTraversal {
            graph: self.graph,
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
            ids,
        }
    }
}
