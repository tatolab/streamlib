// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which machine's clock an inbound mesh link's stamps are taken on, answered
//! for a helper that holds no mesh session of its own: none, because this
//! runtime carries no link from another runtime.

#[cfg(test)]
mod tests;

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};

/// Answer which machine's clock one inbound link's stamps are taken on, for a
/// helper that holds no mesh session of its own.
///
/// The link's name is the source port's mesh address, so a name that is not one
/// is a caller asking about a link from this runtime — which the helper can
/// already answer for itself, and asking here means its two names went astray.
/// An address names no machine: this runtime carries nothing from it.
pub(super) fn handle_inbound_link_stamp_clock_identity(
    rid: String,
    inbound_link_name: &str,
) -> EscalateResponse {
    if let Err(not_an_address) = crate::core::graph::MeshPortAddress::parse(inbound_link_name) {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!(
                "`{inbound_link_name}` is not a mesh address, so no other runtime's clock \
                 answers for it: {not_an_address}"
            ),
        });
    }
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        handle_id: String::new(),
        stamp_clock_identity: None,
        ..Default::default()
    })
}
