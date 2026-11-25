// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One control call parsed, dispatched and answered as its result or its error, the one way
//! `/control` and the REST routes both answer it, so a refusal is the same refusal in the same
//! words over each.

use axum::http::HeaderMap;
use serde_json::{Value, json};
use streamlib_control_contract::{
    CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE, CONTROL_OPERATION_FAILED_ERROR_CODE,
    CONTROL_OPERATION_REFUSED_ERROR_CODE, ControlContractVersionMismatch, ControlOperationCall,
    ControlOperationCallParseRefusal, JSON_RPC_INVALID_PARAMS_ERROR_CODE,
    JSON_RPC_INVALID_REQUEST_ERROR_CODE, JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE,
    JSON_RPC_PARSE_ERROR_CODE,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalOnTheWire, SuggestedOperationCall,
};

mod what_a_control_request_names;

use crate::control_operation_dispatcher::{
    ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal,
    ControlRequestOrigin,
};
pub(crate) use what_a_control_request_names::{
    the_client_name_a_request_sent, the_contract_a_request_names,
};

/// Why a control call was not answered with a result: the error object `/control` carries under
/// `error`, and the body of a REST route's refusal.
#[derive(Debug)]
pub(crate) struct ControlCallError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl ControlCallError {
    pub(crate) fn new(code: i64, message: String, data: Option<Value>) -> Self {
        Self {
            code,
            message,
            data,
        }
    }

    /// The error of a client built against a contract this runtime does not serve.
    pub(crate) fn of_a_contract_version_mismatch(mismatch: ControlContractVersionMismatch) -> Self {
        Self::new(
            CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE,
            mismatch.to_string(),
            serde_json::to_value(mismatch).ok(),
        )
    }

    /// The error of a request body that is not JSON.
    pub(crate) fn of_a_body_that_is_not_json(not_json: &serde_json::Error) -> Self {
        Self::new(
            JSON_RPC_PARSE_ERROR_CODE,
            format!("the request body is not JSON: {not_json}"),
            None,
        )
    }

    /// The error of `refusal`: the runtime's words as its message, and the refusal on the wire
    /// as its `data`.
    pub(crate) fn of_a_refusal(action: &str, refusal: ControlOperationRefusal) -> Self {
        let code = match refusal {
            ControlOperationRefusal::ContractVersionMismatch(mismatch) => {
                return Self::of_a_contract_version_mismatch(mismatch);
            }
            ControlOperationRefusal::RefusedAction(_) => CONTROL_OPERATION_REFUSED_ERROR_CODE,
            ControlOperationRefusal::Coded { .. } => CONTROL_OPERATION_FAILED_ERROR_CODE,
        };
        let runtime_words = refusal.to_string();
        Self::new(
            code,
            runtime_words,
            Some(the_data_of_a_refusal(
                refusal.into_the_refusal_on_the_wire(action),
            )),
        )
    }

    pub(crate) fn code(&self) -> i64 {
        self.code
    }

    /// The `refusal_code` of the refusal the error's `data` carries, when it carries one.
    pub(crate) fn refusal_code(&self) -> Option<&str> {
        self.data.as_ref()?.get("refusal_code")?.as_str()
    }

    /// `{code, message, data}`, `data` left out when there is none.
    pub(crate) fn into_json(self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(data) = self.data {
            error["data"] = data;
        }
        error
    }
}

/// Where a request served once and answered in its response comes from, refused when its
/// contract headers do not read or name a contract this runtime does not serve.
pub(crate) fn the_origin_of_a_request_answered_once(
    dispatcher: &ControlOperationDispatcher,
    request_headers: &HeaderMap,
) -> Result<ControlRequestOrigin, ControlCallError> {
    let client_contract = the_contract_a_request_names(request_headers)
        .map_err(|why| ControlCallError::new(JSON_RPC_INVALID_REQUEST_ERROR_CODE, why, None))?;
    dispatcher
        .refuse_a_client_of_another_contract(client_contract)
        .map_err(ControlCallError::of_a_contract_version_mismatch)?;
    Ok(ControlRequestOrigin {
        client_name: the_client_name_a_request_sent(request_headers),
        subscriptions: None,
        viewers_held_open: None,
    })
}

/// The answer to calling `method_name` with `params`: its result, or why it was not answered.
pub(crate) async fn the_outcome_of_one_control_call(
    dispatcher: &ControlOperationDispatcher,
    request_origin: &ControlRequestOrigin,
    method_name: &str,
    params: Option<Value>,
) -> Result<Value, ControlCallError> {
    let call = ControlOperationCall::parse(method_name, params).map_err(|parse_refusal| {
        ControlCallError::new(
            match parse_refusal {
                ControlOperationCallParseRefusal::NoSuchMethod { .. } => {
                    JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE
                }
                ControlOperationCallParseRefusal::InvalidParams { .. } => {
                    JSON_RPC_INVALID_PARAMS_ERROR_CODE
                }
            },
            parse_refusal.to_string(),
            Some(the_data_of_a_refusal(
                parse_refusal.the_refusal_on_the_wire(),
            )),
        )
    })?;
    dispatcher
        .dispatch(call, request_origin)
        .await
        .map(the_result_of_an_answer)
        .map_err(|refusal| ControlCallError::of_a_refusal(method_name, refusal))
}

fn the_result_of_an_answer(answered: ControlOperationAnswer) -> Value {
    serde_json::to_value(answered)
        .unwrap_or_else(|_| unreachable!("a result type always serializes to JSON"))
}

/// `refusal` as a control error carries it: a suggested call is kept only when the control
/// routes serve its operation, so a discovery tool only an agent reaches is never suggested
/// there. Its params need no second read: `the_suggested_call` makes only a suggestion whose
/// params read.
fn the_data_of_a_refusal(mut refusal: ControlOperationRefusalOnTheWire) -> Value {
    refusal.suggested_call =
        refusal
            .suggested_call
            .take()
            .filter(|SuggestedOperationCall { operation, .. }| {
                streamlib_control_contract::CONTROL_OPERATIONS
                    .iter()
                    .any(|declaration| declaration.method_name == operation)
            });
    refusal.the_refusal_as_json()
}
