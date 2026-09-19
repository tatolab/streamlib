// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    InputLinkPortRef, Link, LinkTraversalMut, OutputLinkPortRef, TraversalSourceMut,
};

use super::super::traversal_source::{LinkLocation, node_index_of};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new edge (link) between two ports.
    ///
    /// Type-safe: `from` must be an output port, `to` must be an input port.
    pub fn add_e(self, from: OutputLinkPortRef, to: InputLinkPortRef) -> LinkTraversalMut<'a> {
        // A source on another runtime has no node here to hang an edge on;
        // `add_link_from_another_runtime` is the door for one.
        let Some(source_on_this_runtime) = from.processor_id_on_this_runtime().cloned() else {
            return self.no_link();
        };
        let Some(from_idx) = node_index_of(self.graph, &source_on_this_runtime) else {
            return self.no_link();
        };
        // A destination on another runtime has no node here either, and no
        // door: the runtime that owns an input is the one that applies the
        // link, so a remote destination is a link *request* and never an edge.
        let Some(destination_on_this_runtime) = to.processor_id_on_this_runtime().cloned() else {
            return self.no_link();
        };
        let Some(to_idx) = node_index_of(self.graph, &destination_on_this_runtime) else {
            return self.no_link();
        };

        if !self.graph[from_idx].has_output(from.port_name()) {
            return self.no_link();
        }
        if !self.graph[to_idx].has_input(to.port_name()) {
            return self.no_link();
        }

        let edge_idx = self
            .graph
            .add_edge(from_idx, to_idx, Link::between(from, to));

        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: vec![LinkLocation::OnAnEdgeOfTheDigraph(edge_idx)],
        }
    }
}
