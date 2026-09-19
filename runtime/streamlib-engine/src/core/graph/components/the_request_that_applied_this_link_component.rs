// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which link request applied a link, and which runtime asked.
//!
//! Carried on the link itself rather than in a registry beside the graph, and
//! that is the whole design: a resend of a request is answered by finding the
//! link that already carries its id, and a link that goes takes its id with it.
//! There is nothing to forget, so nothing leaks on a runtime whose links never
//! reach the compiler.

use crate::core::graph::LinkRequestUniqueId;

/// The link request another runtime applied this link with.
pub struct TheRequestThatAppliedThisLinkComponent {
    /// The requester's id for the request, carried by every resend of it.
    pub link_request_id: LinkRequestUniqueId,
    /// The runtime that asked, which is what the link renders as its creator.
    pub requester_runtime_name: String,
}

impl TheRequestThatAppliedThisLinkComponent {
    /// The record a request leaves on the link it applied.
    pub fn asked_for_by(
        link_request_id: LinkRequestUniqueId,
        requester_runtime_name: impl Into<String>,
    ) -> Self {
        Self {
            link_request_id,
            requester_runtime_name: requester_runtime_name.into(),
        }
    }
}
