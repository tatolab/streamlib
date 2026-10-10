// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    InputLinkPortRef, Link, LinkTraversalMut, OutputLinkPortRef,
    OutputPortInAnotherStreamOnThisMachine, TraversalSourceMut,
};

use super::super::traversal_source::node_index_of;

impl<'a> TraversalSourceMut<'a> {
    /// Add a new edge (link) between two ports.
    ///
    /// Type-safe: `from` must be an output port, `to` must be an input port.
    pub fn add_e(self, from: OutputLinkPortRef, to: InputLinkPortRef) -> LinkTraversalMut<'a> {
        let Some(from_idx) = node_index_of(self.graph, from.processor_id()) else {
            return self.no_link();
        };
        let Some(to_idx) = node_index_of(self.graph, to.processor_id()) else {
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
            ids: vec![edge_idx],
        }
    }

    /// Add a link reading output port `source` of another stream on this
    /// machine into `to`: an edge from `to`'s node to itself, its weight saying
    /// where the source is, since the source's node is in no graph of this
    /// stream.
    pub fn add_e_reading_another_stream(
        self,
        source: OutputPortInAnotherStreamOnThisMachine,
        to: InputLinkPortRef,
    ) -> LinkTraversalMut<'a> {
        let Some(to_idx) = node_index_of(self.graph, to.processor_id()) else {
            return self.no_link();
        };
        if !self.graph[to_idx].has_input(to.port_name()) {
            return self.no_link();
        }

        let edge_idx = self.graph.add_edge(
            to_idx,
            to_idx,
            Link::reading_an_output_port_in_another_stream(source, to),
        );

        LinkTraversalMut {
            graph: self.graph,
            ids: vec![edge_idx],
        }
    }
}
