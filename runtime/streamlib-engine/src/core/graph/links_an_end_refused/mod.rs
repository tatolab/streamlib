// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The links of one node that an end running out of process refused to open, which the engine
//! wires again by itself rather than leave them carrying nothing until the stream starts again.

use super::{Graph, GraphEdgeWithComponents, Link, LinkUniqueId, NodeRunId};
use crate::core::graph::{
    LinkPinTypesDifferComponent, LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent,
    LinkState, LinkStateComponent, OutOfProcessLinkWireRepliesComponent,
};

impl Graph {
    /// Each link into or out of `node_run_id`, or read through it as the reader of a function
    /// node island, that an end out of process refused to open, in the order the graph holds
    /// them, each once.
    pub(crate) fn the_links_of_a_node_an_end_refused(
        &self,
        node_run_id: &NodeRunId,
    ) -> Vec<LinkUniqueId> {
        let mut refused: Vec<LinkUniqueId> = Vec::new();
        let read_through_the_node = |link: &&Link| {
            link.get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
                .is_some_and(|read_through| {
                    read_through
                        .reads_by_island_link
                        .iter()
                        .any(|read| read.reader_input_port.node_run_id() == node_run_id)
                })
        };
        for link in self
            .traversal()
            .v(node_run_id)
            .in_e()
            .iter()
            .chain(self.traversal().v(node_run_id).out_e().iter())
            .chain(self.traversal().e(()).iter().filter(read_through_the_node))
        {
            if an_end_refused_the_link(link) && !refused.contains(&link.id) {
                refused.push(link.id.clone());
            }
        }
        refused
    }
}

/// Whether `link` reads as refused by an end of it that runs out of process: its wiring still
/// awaits those ends' answers, one of which refused it, and its pin types connect. A link the
/// wiring or a disconnect has stamped past pending reads as that stamp, whatever answers it
/// still carries.
pub(crate) fn an_end_refused_the_link(link: &Link) -> bool {
    let stamped = link
        .get::<LinkStateComponent>()
        .map(|state| state.0)
        .unwrap_or(link.state);
    stamped == LinkState::Pending
        && !link.has::<LinkPinTypesDifferComponent>()
        && link
            .get::<OutOfProcessLinkWireRepliesComponent>()
            .is_some_and(OutOfProcessLinkWireRepliesComponent::an_end_refused)
}

#[cfg(test)]
mod tests;
