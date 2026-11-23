// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The control operations over REST on the local API socket: one `POST` route per operation,
//! named by it, its params the JSON body, and the description of every route generated from the
//! contract. The adapter parses, calls the dispatcher and renders, as `/control` does; it holds no
//! operation logic.

use std::sync::LazyLock;

use axum::Json;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use serde_json::Value;
use streamlib_control_contract::{
    control_rest_openapi_document, the_http_status_of_a_control_error,
};
use streamlib_runtime_client_contract::local_api_wire_contract::control_rest_operation_route_path;

use crate::control_operation_dispatcher::ControlOperationDispatcher;
use crate::one_control_call_answered::{
    ControlCallError, the_origin_of_a_request_answered_once, the_outcome_of_one_control_call,
};

/// The REST description, generated from the contract once per process.
static CONTROL_REST_OPENAPI_DOCUMENT: LazyLock<Value> =
    LazyLock::new(control_rest_openapi_document);

/// The route pattern every operation's route matches, the operation's method name captured.
pub(crate) fn control_rest_operation_route_pattern() -> String {
    control_rest_operation_route_path("{operation}")
}

/// Each operation's route: the body is its params, answered in the response.
pub(crate) fn control_rest_operation_route<S>(
    dispatcher: ControlOperationDispatcher,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    post(
        move |Path(operation): Path<String>, request_headers: HeaderMap, request_body: Bytes| {
            answer_one_control_rest_call(
                dispatcher.clone(),
                operation,
                request_headers,
                request_body,
            )
        },
    )
}

/// The description's route.
pub(crate) fn control_rest_openapi_document_route<S>() -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(|| async { Json(&*CONTROL_REST_OPENAPI_DOCUMENT) })
}

async fn answer_one_control_rest_call(
    dispatcher: ControlOperationDispatcher,
    operation: String,
    request_headers: HeaderMap,
    request_body: Bytes,
) -> Response {
    let outcome = async {
        let request_origin = the_origin_of_a_request_answered_once(&dispatcher, &request_headers)?;
        let params = the_params_a_body_holds(&request_body)?;
        the_outcome_of_one_control_call(&dispatcher, &request_origin, &operation, params).await
    }
    .await;
    the_rest_answer_to(outcome)
}

/// A call's outcome over REST: `200` with its result, or its error at the status the error's
/// refusal code, else its code, gives.
fn the_rest_answer_to(outcome: Result<Value, ControlCallError>) -> Response {
    match outcome {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(why) => {
            let status = StatusCode::from_u16(the_http_status_of_a_control_error(
                why.code(),
                why.refusal_code(),
            ))
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(why.into_json())).into_response()
        }
    }
}

/// The params `request_body` holds: none for an empty body.
fn the_params_a_body_holds(request_body: &[u8]) -> Result<Option<Value>, ControlCallError> {
    if request_body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(request_body)
        .map(Some)
        .map_err(|not_json| ControlCallError::of_a_body_that_is_not_json(&not_json))
}

#[cfg(test)]
mod tests;
