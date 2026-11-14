// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A helper-placed node failing itself: the reason it gives over its bridge,
//! held until the engine's lifecycle loop shows the node in error.

use parking_lot::Mutex;

use super::port_stream_facts::WhatTheNodeOfAHelperPlacedProcessorReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestFailTheNode;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;

/// The first reason a node gave for failing itself; later ones are dropped.
#[derive(Debug, Default)]
pub(crate) struct FailureTheNodeReportedOfItself {
    first_reason: Mutex<Option<String>>,
}

impl FailureTheNodeReportedOfItself {
    /// Keep `reason` unless the node already failed.
    pub(crate) fn record(&self, reason: String) {
        self.first_reason.lock().get_or_insert(reason);
    }

    /// Why the node failed itself, once it has.
    pub(crate) fn reason(&self) -> Option<String> {
        self.first_reason.lock().clone()
    }
}

/// Record the node's failure for the lifecycle loop to show.
pub(super) fn handle_fail_the_node(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestFailTheNode,
) -> EscalateResponse {
    what_the_node_reaches
        .failure_the_node_reported_of_itself
        .record(request.reason);
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_reason_a_node_gives_is_the_one_kept() {
        let what_the_node_reaches = WhatTheNodeOfAHelperPlacedProcessorReaches::default();
        assert_eq!(
            what_the_node_reaches
                .failure_the_node_reported_of_itself
                .reason(),
            None
        );
        for (request_id, reason) in [("r-1", "the page stopped rendering"), ("r-2", "later")] {
            let answer = handle_fail_the_node(
                &what_the_node_reaches,
                request_id.to_string(),
                EscalateRequestFailTheNode {
                    reason: reason.to_string(),
                    request_id: request_id.to_string(),
                },
            );
            assert!(matches!(answer, EscalateResponse::Ok(ok) if ok.request_id == request_id));
        }
        assert_eq!(
            what_the_node_reaches
                .failure_the_node_reported_of_itself
                .reason()
                .as_deref(),
            Some("the page stopped rendering")
        );
    }
}
