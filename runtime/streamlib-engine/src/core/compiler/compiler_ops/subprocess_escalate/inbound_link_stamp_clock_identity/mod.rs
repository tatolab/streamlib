// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which machine's clock an inbound mesh link's stamps are taken on, answered
//! for a helper that holds no mesh session of its own.

#[cfg(test)]
mod tests;

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::runtime::mesh::MeshLinkIngressTable;

/// Answer which machine's clock one inbound link's stamps are taken on, for a
/// helper that holds no mesh session of its own.
///
/// The link's name is the source port's mesh address, so a name that is not one
/// is a caller asking about a link from this runtime — which the helper can
/// already answer for itself, and asking here means its two names went astray.
pub(super) fn handle_inbound_link_stamp_clock_identity(
    mesh_link_ingress_table: &MeshLinkIngressTable,
    rid: String,
    inbound_link_name: &str,
) -> EscalateResponse {
    let address = match crate::core::graph::MeshPortAddress::parse(inbound_link_name) {
        Ok(address) => address,
        Err(not_an_address) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "`{inbound_link_name}` is not a mesh address, so no other runtime's clock \
                     answers for it: {not_an_address}"
                ),
            });
        }
    };
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        handle_id: String::new(),
        stamp_clock_identity: mesh_link_ingress_table
            .what_machine_an_address_is_carrying_from(&address)
            .the_machine_if_it_is_known()
            .map(|machine| machine.to_string()),
        ..Default::default()
    })
}
