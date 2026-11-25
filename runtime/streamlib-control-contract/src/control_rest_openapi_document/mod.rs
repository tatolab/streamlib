// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The REST description of the control operations: the contract document rendered as an OpenAPI
//! document, one `POST` route per method, so REST lists no operation, input or answer the
//! contract does not.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_CLIENT_NAME_HEADER_NAME, CONTROL_CONTRACT_REVISION_HEADER_NAME,
    CONTROL_CONTRACT_VERSION_HEADER_NAME, CONTROL_JSON_RPC_ROUTE_PATH,
    CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET, ControlOperationRefusalCode,
    MCP_STDIO_UPGRADE_REQUEST_TARGET, control_rest_operation_route_path,
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

/// The HTTP status a REST route answers a control error of `code` with, whose `data` carries
/// `refusal_code` when the error is a refusal; its body is the error itself, as `/control`
/// carries it under `error`. A refusal code names the status; an error with none, or with one
/// the mapping does not name, answers the status of its code, and a code no mapping names 500.
pub fn the_http_status_of_a_control_error(code: i64, refusal_code: Option<&str>) -> u16 {
    refusal_code
        .and_then(the_http_status_mapped_to_a_refusal_code)
        .or_else(|| the_http_status_mapped_to_a_control_error(code))
        .unwrap_or(500)
}

/// The HTTP status the mapping names for a control error of `code` that carries no refusal code:
/// a call the client can mend is a 4xx, an operation the runtime failed to carry out a 500.
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

/// The HTTP status the mapping names for a refusal of `refusal_code`: 404 for what the call named
/// and the runtime does not hold, 409 for a change that conflicts with what it holds, 400 or 422
/// for a call the client can mend, and 500 for a runtime that failed through no doing of the call.
pub(crate) fn the_http_status_mapped_to_a_refusal_code(refusal_code: &str) -> Option<u16> {
    use ControlOperationRefusalCode::*;
    let refusal_code =
        serde_json::from_value::<ControlOperationRefusalCode>(json!(refusal_code)).ok()?;
    match refusal_code {
        ParamsDoNotMatchSchema => Some(400),
        OperationNotFound | StreamNotFound | NodeNotFound | NodeTypeNotFound | PortNotFound
        | FileNotFound => Some(404),
        NodeNameTaken | LinkAlreadyExists | FileChangedSinceItWasRead => Some(409),
        NodeNotRunning
        | InputTakesNoValue
        | InputValueNotValid
        | LinkPinTypesDoNotConnect
        | LinkNeedsAModuleConversion
        | InputTakesNoWire
        | LinkStreamsIntoASetting
        | LinkFromAFunctionNodeIntoASetting
        | Refused => Some(422),
        RuntimeFailed | CheckRanOutOfTime | ExtensionSdkNotFound => Some(500),
        _ => None,
    }
}

/// The refusal codes the contract lists.
pub(crate) fn the_refusal_codes_the_contract_lists(contract: &Value) -> Vec<String> {
    contract["components"]["schemas"]["ControlOperationRefusalCode"]["oneOf"]
        .as_array()
        .expect("the contract lists the refusal codes")
        .iter()
        .map(|refusal_code| {
            refusal_code["const"]
                .as_str()
                .expect("each refusal code the contract lists is a string")
                .to_owned()
        })
        .collect()
}

/// The control operations over REST, as one OpenAPI document generated from the contract.
pub fn control_rest_openapi_document() -> Value {
    the_rest_description_of(&control_contract_document())
}

fn the_rest_description_of(contract: &Value) -> Value {
    let refusal_statuses = the_statuses_a_refusal_may_answer(contract);
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
                json!({ "post": the_rest_operation_of(method, &refusal_statuses) }),
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
            "description": "Every control operation over REST: `POST` its params as the JSON body to its route, an empty body sending none. It answers `200` with its result, or the status its refusal's `refusal_code` gives, else its error's code, with the error `/control` carries under `error`: `code`, `message` (the runtime's words) and `data` (the refusal). Generated from the control contract, which describes each operation's params, result and errors.",
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

fn the_rest_operation_of(method: &Value, refusal_statuses: &BTreeSet<u16>) -> Value {
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
    for (status, error_schemas) in the_error_schemas_by_status(method, refusal_statuses) {
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

/// The statuses a refusal may answer, one for each refusal code the contract lists.
pub(crate) fn the_statuses_a_refusal_may_answer(contract: &Value) -> BTreeSet<u16> {
    the_refusal_codes_the_contract_lists(contract)
        .iter()
        .map(|refusal_code| {
            the_http_status_mapped_to_a_refusal_code(refusal_code)
                .expect("the mapping names each refusal code the contract lists")
        })
        .collect()
}

/// The statuses a route may answer `error` with: each a refusal may answer when its `data` is a
/// refusal, which carries its `refusal_code`, else the one its code gives.
pub(crate) fn the_statuses_an_error_may_answer(
    error: &Value,
    refusal_statuses: &BTreeSet<u16>,
) -> BTreeSet<u16> {
    let carries_a_refusal_code = error["data"]["$ref"]
        .as_str()
        .is_some_and(|reference| reference.ends_with(&format!("/{REFUSAL_SCHEMA_NAME}")));
    if carries_a_refusal_code {
        return refusal_statuses.clone();
    }
    let code = error["code"]
        .as_i64()
        .expect("each error the contract states has a whole-number code");
    BTreeSet::from([the_http_status_of_a_control_error(code, None)])
}

/// The schema of the refusal an error's `data` carries, `refusal_code` among its fields.
const REFUSAL_SCHEMA_NAME: &str = "ControlOperationRefusalOnTheWire";

/// The error bodies a route may answer, by the HTTP status each answers with: the method's own
/// errors, then the two any request may meet before it names one call.
fn the_error_schemas_by_status(
    method: &Value,
    refusal_statuses: &BTreeSet<u16>,
) -> BTreeMap<u16, Vec<Value>> {
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
        let schema = json!({
            "type": "object",
            "required": ["code", "message"],
            "properties": properties,
        });
        for status in the_statuses_an_error_may_answer(&error, refusal_statuses) {
            by_status.entry(status).or_default().push(schema.clone());
        }
    }
    by_status
}

#[cfg(test)]
mod tests;
