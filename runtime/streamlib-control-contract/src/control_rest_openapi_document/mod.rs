// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The REST description of the control operations: the contract document rendered as an OpenAPI
//! document, one `POST` route per method, so REST lists no operation, input or answer the
//! contract does not.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_CLIENT_NAME_HEADER_NAME, CONTROL_CONTRACT_REVISION_HEADER_NAME,
    CONTROL_CONTRACT_VERSION_HEADER_NAME, CONTROL_JSON_RPC_ROUTE_PATH,
    CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET, MCP_STDIO_UPGRADE_REQUEST_TARGET,
    control_rest_operation_route_path,
};

use crate::control_contract_document::{
    CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE, CONTROL_OPERATION_FAILED_ERROR_CODE,
    CONTROL_OPERATION_REFUSED_ERROR_CODE, JSON_RPC_INVALID_PARAMS_ERROR_CODE,
    JSON_RPC_INVALID_REQUEST_ERROR_CODE, JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE,
    JSON_RPC_PARSE_ERROR_CODE, control_contract_document,
};
use crate::control_operations::CONTROL_OPERATIONS;

/// The OpenAPI version the REST description is written in.
pub const OPENAPI_DOCUMENT_VERSION: &str = "3.1.0";

const JSON_MEDIA_TYPE: &str = "application/json";

/// The header parameters every route takes, each by its component name, header, JSON Schema type
/// and description.
const CONTROL_REST_HEADER_PARAMETERS: &[(&str, &str, &str, &str)] = &[
    (
        "ControlContractVersion",
        CONTROL_CONTRACT_VERSION_HEADER_NAME,
        "integer",
        "The contract version the client was built against; another version is refused with error 3.",
    ),
    (
        "ControlContractRevision",
        CONTROL_CONTRACT_REVISION_HEADER_NAME,
        "integer",
        "The contract revision the client was built against, read as 0 when absent; a higher revision is refused with error 3.",
    ),
    (
        "ControlClientName",
        CONTROL_CLIENT_NAME_HEADER_NAME,
        "string",
        "The client's name, which a change it makes with no author is recorded under.",
    ),
];

/// The HTTP status a REST route answers a control error of `code` with; its body is the error
/// itself, as `/control` carries it under `error`. A code no mapping names answers 500.
pub fn the_http_status_of_a_control_error(code: i64) -> u16 {
    the_http_status_mapped_to_a_control_error(code).unwrap_or(500)
}

/// The HTTP status the mapping names for a control error of `code`: a call the client can mend is
/// a 4xx, an operation the runtime failed to carry out a 500.
pub(crate) fn the_http_status_mapped_to_a_control_error(code: i64) -> Option<u16> {
    match code {
        JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE => Some(404),
        JSON_RPC_INVALID_PARAMS_ERROR_CODE
        | JSON_RPC_PARSE_ERROR_CODE
        | JSON_RPC_INVALID_REQUEST_ERROR_CODE => Some(400),
        CONTROL_OPERATION_REFUSED_ERROR_CODE => Some(422),
        CONTROL_OPERATION_FAILED_ERROR_CODE => Some(500),
        CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE => Some(409),
        _ => None,
    }
}

/// The control operations over REST, as one OpenAPI document generated from the contract.
pub fn control_rest_openapi_document() -> Value {
    the_rest_description_of(&control_contract_document())
}

fn the_rest_description_of(contract: &Value) -> Value {
    let paths: Map<String, Value> = contract["methods"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|method| {
            let method_name = method["name"]
                .as_str()
                .expect("the contract names each method");
            (
                control_rest_operation_route_path(method_name),
                json!({ "post": the_rest_operation_of(method) }),
            )
        })
        .collect();
    let parameters: Map<String, Value> = CONTROL_REST_HEADER_PARAMETERS
        .iter()
        .map(|(component_name, header_name, schema_type, description)| {
            (
                (*component_name).to_owned(),
                json!({
                    "name": header_name,
                    "in": "header",
                    "required": false,
                    "description": description,
                    "schema": { "type": schema_type },
                }),
            )
        })
        .collect();
    let info = &contract["info"];
    json!({
        "openapi": OPENAPI_DOCUMENT_VERSION,
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "info": {
            "title": info["title"],
            "version": info["version"],
            "description": "Every control operation over REST: `POST` its params as the JSON body to its route, an empty body sending none. It answers `200` with its result, or the status its error's code gives with the error `/control` carries under `error`: `code`, `message` (the runtime's words) and `data` (the refusal). Generated from the control contract, which describes each operation's params, result and errors.",
            "x-contract-version": info["x-contract-version"],
            "x-contract-revision": info["x-contract-revision"],
        },
        "paths": paths,
        "components": {
            "schemas": contract["components"]["schemas"],
            "parameters": parameters,
        },
    })
}

fn the_rest_operation_of(method: &Value) -> Value {
    let example = method["examples"].get(0);
    let mut request_media = json!({ "schema": method["params"][0]["schema"] });
    if let Some(params) = example.and_then(|example| example["params"][0].get("value")) {
        request_media["example"] = params.clone();
    }
    let mut answer_media = json!({ "schema": method["result"]["schema"] });
    if let Some(answer) = example.and_then(|example| example["result"].get("value")) {
        answer_media["example"] = answer.clone();
    }
    let mut responses = Map::new();
    responses.insert(
        "200".to_owned(),
        json!({
            "description": "The operation's result.",
            "content": { JSON_MEDIA_TYPE: answer_media },
        }),
    );
    for (status, error_schemas) in the_error_schemas_by_status(method) {
        let schema = match <[Value; 1]>::try_from(error_schemas) {
            Ok([one]) => one,
            Err(several) => json!({ "oneOf": several }),
        };
        responses.insert(
            status.to_string(),
            json!({
                "description": "The call was not served: the error `/control` carries under `error`.",
                "content": { JSON_MEDIA_TYPE: { "schema": schema } },
            }),
        );
    }
    let mut operation = json!({
        "operationId": method["name"],
        "summary": method["summary"],
        "description": method["description"],
        "tags": [method["x-group"]],
        "parameters": CONTROL_REST_HEADER_PARAMETERS
            .iter()
            .map(|(component_name, _, _, _)| {
                json!({ "$ref": format!("#/components/parameters/{component_name}") })
            })
            .collect::<Vec<_>>(),
        "requestBody": {
            "required": false,
            "description": "The operation's params; an empty body sends none.",
            "content": { JSON_MEDIA_TYPE: request_media },
        },
        "responses": responses,
    });
    if method.get("x-replaced-by").is_some() {
        operation["deprecated"] = json!(true);
    }
    if needs_a_connection_that_stays_open(method) {
        operation["x-needs-a-connection"] = json!(true);
        operation["description"] = json!(format!(
            "{} It answers only on a connection that stays open, `{CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET}` or `{MCP_STDIO_UPGRADE_REQUEST_TARGET}`; over REST it is refused, as over `POST {CONTROL_JSON_RPC_ROUTE_PATH}`.",
            method["description"]
                .as_str()
                .expect("the contract describes each method")
        ));
    }
    for (key, value) in method.as_object().into_iter().flatten() {
        if key.starts_with("x-") {
            operation[key] = value.clone();
        }
    }
    operation
}

/// Whether the operation table declares `method` served only on a connection that stays open.
fn needs_a_connection_that_stays_open(method: &Value) -> bool {
    CONTROL_OPERATIONS.iter().any(|declaration| {
        declaration.method_name == method["name"] && declaration.needs_a_connection_that_stays_open
    })
}

/// The error bodies a route may answer, by the HTTP status each answers with: the method's own
/// errors, then the two any request may meet before it names one call.
fn the_error_schemas_by_status(method: &Value) -> BTreeMap<u16, Vec<Value>> {
    let mut by_status = BTreeMap::<u16, Vec<Value>>::new();
    let method_errors = method["errors"].as_array().into_iter().flatten().cloned();
    let request_errors = [
        json!({ "code": JSON_RPC_PARSE_ERROR_CODE, "message": "the body is not JSON" }),
        json!({
            "code": JSON_RPC_INVALID_REQUEST_ERROR_CODE,
            "message": "a contract header does not read",
        }),
    ];
    for error in method_errors.chain(request_errors) {
        let code = error["code"]
            .as_i64()
            .expect("each error the contract states has a whole-number code");
        let mut properties = json!({
            "code": { "const": code },
            "message": { "type": "string", "description": error["message"] },
        });
        if let Some(data) = error.get("data") {
            properties["data"] = data.clone();
        }
        by_status
            .entry(the_http_status_of_a_control_error(code))
            .or_default()
            .push(json!({
                "type": "object",
                "required": ["code", "message"],
                "properties": properties,
            }));
    }
    by_status
}

#[cfg(test)]
mod tests;
