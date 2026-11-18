// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a control request's headers name: the client, and the contract it was built against.

use axum::http::HeaderMap;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_CLIENT_NAME_HEADER_NAME, CONTROL_CONTRACT_REVISION_HEADER_NAME,
    CONTROL_CONTRACT_VERSION_HEADER_NAME,
};

use crate::control_operation_dispatcher::ContractAClientNames;

/// The client's name, as the request's header names it.
pub(super) fn the_client_name_a_request_sent(request_headers: &HeaderMap) -> Option<String> {
    request_headers
        .get(CONTROL_CLIENT_NAME_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// The contract the request's headers name, refused when a header is not a whole number.
pub(super) fn the_contract_a_request_names(
    request_headers: &HeaderMap,
) -> Result<ContractAClientNames, String> {
    Ok(ContractAClientNames::named(
        the_whole_number_a_header_names(
            request_headers,
            CONTROL_CONTRACT_VERSION_HEADER_NAME,
            "a contract version",
        )?,
        the_whole_number_a_header_names(
            request_headers,
            CONTROL_CONTRACT_REVISION_HEADER_NAME,
            "a contract revision",
        )?,
    ))
}

fn the_whole_number_a_header_names(
    request_headers: &HeaderMap,
    header_name: &str,
    what_it_names: &str,
) -> Result<Option<u32>, String> {
    let Some(value) = request_headers.get(header_name) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map(Some)
        .ok_or_else(|| format!("`{header_name}` must be {what_it_names}, a whole number"))
}
