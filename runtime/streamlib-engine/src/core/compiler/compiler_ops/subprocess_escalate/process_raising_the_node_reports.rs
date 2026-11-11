// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A helper-placed node's `process()` raising: what it raised, held from the
//! helper's report until the engine's lifecycle loop shows it beside the node's
//! state, and cleared when the helper reports a clean run again.

use parking_lot::Mutex;

use super::port_stream_facts::WhatTheNodeOfAHelperPlacedProcessorReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestSetProcessRaisingBecause;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;

/// What the node's `process()` raised, as its helper last reported; `None`
/// while it runs cleanly.
#[derive(Debug, Default)]
pub(crate) struct ProcessRaisingTheNodeReports {
    raising_because: Mutex<Option<String>>,
}

impl ProcessRaisingTheNodeReports {
    /// Keep `raising_because`, or forget the raise for `None`.
    pub(crate) fn record(&self, raising_because: Option<String>) {
        *self.raising_because.lock() = raising_because;
    }

    /// What `process()` raised, while it has not run cleanly since.
    pub(crate) fn raising_because(&self) -> Option<String> {
        self.raising_because.lock().clone()
    }
}

/// Record what the node's `process()` raised, or that it runs cleanly again.
pub(super) fn handle_set_process_raising_because(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestSetProcessRaisingBecause,
) -> EscalateResponse {
    what_the_node_reaches
        .process_raising_the_node_reports
        .record(request.raising_because);
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_process_raising_because(
        what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
        request_id: &str,
        raising_because: Option<&str>,
    ) {
        let answer = handle_set_process_raising_because(
            what_the_node_reaches,
            request_id.to_string(),
            EscalateRequestSetProcessRaisingBecause {
                raising_because: raising_because.map(str::to_string),
                request_id: request_id.to_string(),
            },
        );
        assert!(matches!(answer, EscalateResponse::Ok(_)), "{answer:?}");
    }

    #[test]
    fn a_raise_is_held_until_the_helper_reports_a_clean_run() {
        let what_the_node_reaches = WhatTheNodeOfAHelperPlacedProcessorReaches::default();
        assert_eq!(
            what_the_node_reaches
                .process_raising_the_node_reports
                .raising_because(),
            None
        );

        set_process_raising_because(
            &what_the_node_reaches,
            "r-1",
            Some("process() raised RuntimeError: broke on purpose"),
        );
        assert_eq!(
            what_the_node_reaches
                .process_raising_the_node_reports
                .raising_because()
                .as_deref(),
            Some("process() raised RuntimeError: broke on purpose")
        );

        set_process_raising_because(&what_the_node_reaches, "r-2", None);
        assert_eq!(
            what_the_node_reaches
                .process_raising_the_node_reports
                .raising_because(),
            None
        );
    }
}
