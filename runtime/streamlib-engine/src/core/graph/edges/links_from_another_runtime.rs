// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The links whose source is a port on another runtime.
//!
//! A link is an edge of the digraph, which needs a node at each end. A link
//! carrying from another runtime has no node here to hang its source on — the
//! bags arrive through that address's ingress, not out of a local processor —
//! so those links are kept here instead, beside the digraph rather than in it.
//!
//! Nothing else about them is special: the graph traversal reaches them exactly
//! as it reaches an edge, so a destination's `in_e()` sees them and the fan-in
//! cap, the windowed-port refusal and the inbound-link enumeration count them
//! without any of those having to remember they exist. The one traversal that
//! never yields one is `out_e()`, because no local node produces them.

use crate::core::graph::{InputLinkPortRef, Link, LinkUniqueId, ProcessorUniqueId};

/// Every link on this runtime whose source is a port on another runtime, in
/// the order they were connected.
#[derive(Debug, Default)]
pub struct LinksFromAnotherRuntime(Vec<Link>);

impl LinksFromAnotherRuntime {
    /// Keep `link`, whose source is a port on another runtime.
    pub(in crate::core::graph) fn keep(&mut self, link: Link) -> &Link {
        self.0.push(link);
        self.0.last().expect("the link just pushed is there")
    }

    /// The link with this id, if it is one of these.
    pub(in crate::core::graph) fn get(&self, link_id: &LinkUniqueId) -> Option<&Link> {
        self.0.iter().find(|link| &link.id == link_id)
    }

    /// The link with this id, to be changed.
    pub(in crate::core::graph) fn get_mut(&mut self, link_id: &LinkUniqueId) -> Option<&mut Link> {
        self.0.iter_mut().find(|link| &link.id == link_id)
    }

    /// Forget the link with this id, answering whether there was one.
    pub(in crate::core::graph) fn forget(&mut self, link_id: &LinkUniqueId) -> bool {
        let before = self.0.len();
        self.0.retain(|link| &link.id != link_id);
        self.0.len() != before
    }

    /// Forget every link into `destination_processor_id` — what a node's own
    /// removal does to the edges the digraph cascades.
    pub(in crate::core::graph) fn forget_every_link_into(
        &mut self,
        destination_processor_id: &ProcessorUniqueId,
    ) {
        self.0
            .retain(|link| &link.to_port().processor_id != destination_processor_id);
    }

    /// Every one of these links, in connect order.
    pub(crate) fn every_link(&self) -> impl Iterator<Item = &Link> {
        self.0.iter()
    }

    /// Every one of these links carrying into `destination`'s processor.
    pub(in crate::core::graph) fn every_link_into(
        &self,
        destination_processor_id: &ProcessorUniqueId,
    ) -> impl Iterator<Item = &Link> {
        self.0
            .iter()
            .filter(move |link| &link.to_port().processor_id == destination_processor_id)
    }

    /// Whether a link already carries into exactly `destination` — the check
    /// `connect` makes before keeping a second one.
    pub(in crate::core::graph) fn already_carries_into(
        &self,
        destination: &InputLinkPortRef,
    ) -> bool {
        self.0.iter().any(|link| link.to_port() == destination)
    }
}
