// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A function node raising inside the read of a helper-placed node: what its
//! call raised, shown on the function node itself while the reader keeps
//! running, and cleared when the helper reports a clean call again.

use super::port_stream_facts::WhatAHelperPlacedNodeReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestSetFunctionNodeRaisingBecause;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;

/// Record what a function node the helper's islands call raised, or that its
/// calls run cleanly again. A node its islands no longer call — an island
/// unwired while the report was on its way — is passed over.
pub(super) fn handle_set_function_node_raising_because(
    what_the_node_reaches: &WhatAHelperPlacedNodeReaches,
    rid: String,
    request: EscalateRequestSetFunctionNodeRaisingBecause,
) -> EscalateResponse {
    what_the_node_reaches
        .how_its_islands_ran_each_function_node
        .note_why_the_helper_finds_it_raising(&request.function_node, request.raising_because);
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests;
