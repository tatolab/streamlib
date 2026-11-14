// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A helper-placed node reporting itself not ready: why, held from the helper's
//! report until the engine's lifecycle loop shows it beside the node's state,
//! and cleared when the helper reports the node ready again.

use parking_lot::Mutex;

use super::port_stream_facts::WhatTheNodeOfAHelperPlacedProcessorReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestSetNotReadyBecause;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;

/// Why the node is not ready, as its helper last reported; `None` while it is.
#[derive(Debug, Default)]
pub(crate) struct NodeNotReadyReports {
    not_ready_because: Mutex<Option<String>>,
}

impl NodeNotReadyReports {
    /// Keep `not_ready_because`, or forget it for `None`.
    pub(crate) fn record(&self, not_ready_because: Option<String>) {
        *self.not_ready_because.lock() = not_ready_because;
    }

    /// Why the node is not ready, while its helper says so.
    pub(crate) fn not_ready_because(&self) -> Option<String> {
        self.not_ready_because.lock().clone()
    }
}

/// Record why the node is not ready, or that it is ready again.
pub(super) fn handle_set_not_ready_because(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestSetNotReadyBecause,
) -> EscalateResponse {
    what_the_node_reaches
        .node_not_ready_reports
        .record(request.not_ready_because);
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_not_ready_because(
        what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
        request_id: &str,
        not_ready_because: Option<&str>,
    ) {
        let answer = handle_set_not_ready_because(
            what_the_node_reaches,
            request_id.to_string(),
            EscalateRequestSetNotReadyBecause {
                not_ready_because: not_ready_because.map(str::to_string),
                request_id: request_id.to_string(),
            },
        );
        assert!(matches!(answer, EscalateResponse::Ok(_)), "{answer:?}");
    }

    #[test]
    fn a_not_ready_report_is_held_until_the_helper_reports_the_node_ready() {
        let what_the_node_reaches = WhatTheNodeOfAHelperPlacedProcessorReaches::default();
        assert_eq!(
            what_the_node_reaches
                .node_not_ready_reports
                .not_ready_because(),
            None
        );

        set_not_ready_because(&what_the_node_reaches, "r-1", Some("`riv_path` is not set"));
        assert_eq!(
            what_the_node_reaches
                .node_not_ready_reports
                .not_ready_because()
                .as_deref(),
            Some("`riv_path` is not set")
        );

        set_not_ready_because(&what_the_node_reaches, "r-2", None);
        assert_eq!(
            what_the_node_reaches
                .node_not_ready_reports
                .not_ready_because(),
            None
        );
    }
}
