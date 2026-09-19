// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    InputLinkPortRef, Link, LinkTraversalMut, MeshPortAddress, TraversalSourceMut,
};

use super::super::traversal_source::{LinkLocation, node_index_of};

impl<'a> TraversalSourceMut<'a> {
    /// Add a link carrying from a port on another runtime into a local input.
    ///
    /// The digraph is not where it goes: an edge needs a node at each end, and
    /// the source is on another machine. It is kept beside the digraph, which
    /// is where every link traversal but `out_e()` reads it from.
    ///
    /// The destination is validated here the way `add_e` validates its own; an
    /// unknown destination or port yields an empty traversal.
    pub fn add_link_from_another_runtime(
        self,
        source: MeshPortAddress,
        destination: InputLinkPortRef,
    ) -> LinkTraversalMut<'a> {
        // Both ends on other runtimes is no link of this runtime's: it is a
        // third-party wiring, which reaches the runtime owning the input as a
        // request and becomes an ordinary link from another runtime there.
        let destination_exists = destination
            .processor_id_on_this_runtime()
            .and_then(|processor_id| node_index_of(self.graph, processor_id))
            .is_some_and(|node_idx| self.graph[node_idx].has_input(destination.port_name()));
        if !destination_exists {
            return self.no_link();
        }

        let link_id = self
            .links_from_another_runtime
            .keep(Link::between(
                crate::core::graph::OutputLinkPortRef::on_another_runtime(source),
                destination,
            ))
            .id
            .clone();

        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: vec![LinkLocation::AmongTheLinksFromAnotherRuntime(link_id)],
        }
    }
}
