// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The links of one node that an end running out of process refused to open, which the engine
//! wires again by itself rather than leave them carrying nothing until the stream starts again.

use super::{Graph, GraphEdgeWithComponents, Link, LinkUniqueId, NodeRunId};
use crate::core::graph::OutOfProcessLinkWireRepliesComponent;

impl Graph {
    /// Each link into or out of `node_run_id` that an end out of process refused to open, in
    /// the order the graph holds them, each once.
    pub(crate) fn the_links_of_a_node_an_end_refused(
        &self,
        node_run_id: &NodeRunId,
    ) -> Vec<LinkUniqueId> {
        let into_the_node = self.traversal().v(node_run_id).in_e().ids();
        let out_of_the_node = self.traversal().v(node_run_id).out_e().ids();
        let mut refused: Vec<LinkUniqueId> = Vec::new();
        for link_id in into_the_node.into_iter().chain(out_of_the_node) {
            let is_refused = self
                .traversal()
                .e(&link_id)
                .first()
                .is_some_and(an_end_refused_the_link);
            if is_refused && !refused.contains(&link_id) {
                refused.push(link_id);
            }
        }
        refused
    }
}

/// Whether an end of `link` that runs out of process refused to open its side of it.
pub(crate) fn an_end_refused_the_link(link: &Link) -> bool {
    link.get::<OutOfProcessLinkWireRepliesComponent>()
        .is_some_and(OutOfProcessLinkWireRepliesComponent::an_end_refused)
}

#[cfg(test)]
mod tests;
