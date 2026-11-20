// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{
    InputLinkPortRef, Link, LinkTraversalMut, OutputLinkPortRef, TraversalSourceMut,
};

use super::super::traversal_source::node_index_of;

impl<'a> TraversalSourceMut<'a> {
    /// Add a new edge (link) between two ports, each end under its port's current name and
    /// keeping the member of a bundle it starts or ends at.
    ///
    /// Type-safe: `from` must be an output port, `to` must be an input port.
    pub fn add_e(self, from: OutputLinkPortRef, to: InputLinkPortRef) -> LinkTraversalMut<'a> {
        let Some(from_idx) = node_index_of(self.graph, from.node_run_id()) else {
            return self.no_link();
        };
        let Some(to_idx) = node_index_of(self.graph, to.node_run_id()) else {
            return self.no_link();
        };

        crate::core::graph::grow_a_pin_group_to_hold_the_port(
            self.graph,
            from_idx,
            crate::core::PortDirection::Output,
            from.port_name(),
        );
        crate::core::graph::grow_a_pin_group_to_hold_the_port(
            self.graph,
            to_idx,
            crate::core::PortDirection::Input,
            to.port_name(),
        );
        let Some(from) = self.graph[from_idx]
            .try_output(from.port_name())
            .map(|current| current.at_member(from.member().map(str::to_owned)))
        else {
            return self.no_link();
        };
        let Some(to) = self.graph[to_idx]
            .try_input(to.port_name())
            .map(|current| current.at_member(to.member().map(str::to_owned)))
        else {
            return self.no_link();
        };

        let edge_idx = self
            .graph
            .add_edge(from_idx, to_idx, Link::between(from, to));
        for node_index in [from_idx, to_idx] {
            crate::core::graph::reshape_the_pin_group_slots_of(self.graph, node_index);
        }

        LinkTraversalMut {
            graph: self.graph,
            ids: vec![edge_idx],
        }
    }
}
