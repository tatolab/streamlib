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
    /// The revision, read as 0 when absent.
    pub(crate) revision: Option<u32>,
}

impl ContractAClientNames {
    /// The side built against the older contract when a runtime serving `runtime_version` and
    /// `runtime_revision` refuses this client: one whose version differs, or whose revision is
    /// higher. A lower revision is served, since within a version shapes change only by addition.
    fn the_older_side_against(
        self,
        runtime_version: u32,
        runtime_revision: u32,
    ) -> Option<OlderSide> {
        let client_version = self.version?;
        let client_revision = self.revision.unwrap_or_default();
        if client_version < runtime_version {
            Some(OlderSide::Client)
        } else if client_version > runtime_version || client_revision > runtime_revision {
            Some(OlderSide::Runtime)
        } else {
            None
        }
    }
}

impl ControlOperationDispatcher {
    /// Refuse a client built against a contract this runtime does not serve, naming the older
    /// side.
    pub(crate) fn refuse_a_client_of_another_contract(
        &self,
        client: ContractAClientNames,
    ) -> Result<(), ControlContractVersionMismatch> {
        let Some(older_side) =
            client.the_older_side_against(CONTROL_CONTRACT_VERSION, CONTROL_CONTRACT_REVISION)
        else {
            return Ok(());
        };
        Err(ControlContractVersionMismatch {
            client_contract_version: client.version.unwrap_or_default(),
            runtime_contract_version: CONTROL_CONTRACT_VERSION,
            client_contract_revision: client.revision.unwrap_or_default(),
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
mod tests {
    use super::*;

    const RUNTIME_VERSION: u32 = 3;
    const RUNTIME_REVISION: u32 = 2;

    fn the_older_side_of(version: Option<u32>, revision: Option<u32>) -> Option<OlderSide> {
        ContractAClientNames { version, revision }
            .the_older_side_against(RUNTIME_VERSION, RUNTIME_REVISION)
    }

    #[test]
    fn a_client_naming_no_version_is_served_whatever_its_revision() {
        assert_eq!(the_older_side_of(None, None), None);
        assert_eq!(the_older_side_of(None, Some(RUNTIME_REVISION + 1)), None);
    }

    #[test]
    fn a_client_of_the_same_version_and_a_revision_no_higher_is_served() {
        assert_eq!(the_older_side_of(Some(RUNTIME_VERSION), None), None);
        assert_eq!(the_older_side_of(Some(RUNTIME_VERSION), Some(0)), None);
        assert_eq!(
            the_older_side_of(Some(RUNTIME_VERSION), Some(RUNTIME_REVISION)),
            None
        );
    }

    #[test]
    fn a_client_of_a_higher_revision_finds_the_runtime_older() {
        assert_eq!(
            the_older_side_of(Some(RUNTIME_VERSION), Some(RUNTIME_REVISION + 1)),
            Some(OlderSide::Runtime)
        );
    }

    #[test]
    fn a_client_of_another_version_finds_the_side_of_the_lower_version_older() {
        assert_eq!(
            the_older_side_of(Some(RUNTIME_VERSION + 1), Some(0)),
            Some(OlderSide::Runtime)
        );
        assert_eq!(
            the_older_side_of(Some(RUNTIME_VERSION - 1), Some(RUNTIME_REVISION + 5)),
            Some(OlderSide::Client)
        );
    }
}
