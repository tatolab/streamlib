// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The contract a client names it was built against, and the refusal of one this runtime does
//! not serve.

use streamlib_control_contract::{
    CONTROL_CONTRACT_REVISION, CONTROL_CONTRACT_VERSION, ControlContractVersionMismatch, OlderSide,
};

use super::ControlOperationDispatcher;

/// The contract version and revision a client names, in `discover`'s params or in the headers of
/// a request or a WebSocket upgrade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContractAClientNames {
    /// The version, absent from a caller that names none, such as an agent reading
    /// `describe_operations`; such a caller is always served.
    pub(crate) version: Option<u32>,
    /// The revision, 0 for a client that names none.
    pub(crate) revision: u32,
}

impl ContractAClientNames {
    /// The contract a client names by `version` and `revision`, a revision it names none of
    /// read as 0.
    pub(crate) fn named(version: Option<u32>, revision: Option<u32>) -> Self {
        Self {
            version,
            revision: revision.unwrap_or_default(),
        }
    }
}

/// The side built against the older contract when a runtime serving `runtime_version` and
/// `runtime_revision` refuses a client of `client_version` and `client_revision`: one whose
/// version differs, or whose revision is higher. A lower revision is served, since within a
/// version shapes change only by addition.
fn the_older_side_of(
    client_version: u32,
    client_revision: u32,
    runtime_version: u32,
    runtime_revision: u32,
) -> Option<OlderSide> {
    if client_version < runtime_version {
        Some(OlderSide::Client)
    } else if client_version > runtime_version || client_revision > runtime_revision {
        Some(OlderSide::Runtime)
    } else {
        None
    }
}

impl ControlOperationDispatcher {
    /// Refuse a client built against a contract this runtime does not serve, naming the older
    /// side.
    pub(crate) fn refuse_a_client_of_another_contract(
        &self,
        client: ContractAClientNames,
    ) -> Result<(), ControlContractVersionMismatch> {
        let Some(client_version) = client.version else {
            return Ok(());
        };
        let Some(older_side) = the_older_side_of(
            client_version,
            client.revision,
            CONTROL_CONTRACT_VERSION,
            CONTROL_CONTRACT_REVISION,
        ) else {
            return Ok(());
        };
        Err(ControlContractVersionMismatch {
            client_contract_version: client_version,
            runtime_contract_version: CONTROL_CONTRACT_VERSION,
            client_contract_revision: client.revision,
            runtime_contract_revision: CONTROL_CONTRACT_REVISION,
            older_side,
            runtime_version: self
                .operations_on_the_loaded_streams
                .describe_this_runtime()
                .version,
        })
    }
}

#[cfg(test)]
mod tests;
