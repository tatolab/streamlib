// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn the_operation_at<'document>(document: &'document Value, method_name: &str) -> &'document Value {
    &document["paths"][control_rest_operation_route_path(method_name)]["post"]
}

#[test]
fn each_contract_method_is_one_post_route_and_no_route_is_anything_else() {
    let document = control_rest_openapi_document();
    let paths = document["paths"]
        .as_object()
        .unwrap_or_else(|| panic!("`paths` is an object"));
    let mut described: Vec<(&str, Vec<&str>)> = paths
        .iter()
        .map(|(path, item)| {
            (
                path.as_str(),
                item.as_object()
                    .into_iter()
                    .flatten()
                    .map(|(method, _)| method.as_str())
                    .collect(),
            )
        })
        .collect();
    described.sort_unstable();
    let mut registered: Vec<String> = CONTROL_OPERATIONS
        .iter()
        .map(|declaration| control_rest_operation_route_path(declaration.method_name))
        .collect();
    registered.sort_unstable();
    assert_eq!(
        described,
        registered
            .iter()
            .map(|path| (path.as_str(), vec!["post"]))
            .collect::<Vec<_>>()
    );
}

#[test]
fn each_route_takes_the_params_and_answers_the_result_its_contract_method_states() {
    let contract = control_contract_document();
    let document = the_rest_description_of(&contract);
    for method in contract["methods"].as_array().into_iter().flatten() {
        let method_name = method["name"].as_str().expect("a method name");
        let operation = the_operation_at(&document, method_name);
        assert_eq!(operation["operationId"], method["name"]);
        assert!(
            operation["description"]
                .as_str()
                .is_some_and(|described| described
                    .starts_with(method["description"].as_str().expect("a description"))),
            "{method_name}"
        );
        assert_eq!(
            operation["requestBody"]["content"][JSON_MEDIA_TYPE]["schema"],
            method["params"][0]["schema"],
            "{method_name}"
        );
        assert_eq!(
            operation["responses"]["200"]["content"][JSON_MEDIA_TYPE]["schema"],
            method["result"]["schema"],
            "{method_name}"
        );
        assert_eq!(operation["x-effect"], method["x-effect"], "{method_name}");
        assert_eq!(
            operation.get("deprecated").is_some(),
            method.get("x-replaced-by").is_some(),
            "{method_name}"
        );
    }
    assert_eq!(
        document["components"]["schemas"],
        contract["components"]["schemas"]
    );
    assert_eq!(
        document["info"]["x-contract-revision"],
        contract["info"]["x-contract-revision"]
    );
}

#[test]
fn each_control_error_code_answers_its_own_status() {
    for (code, status) in [
        (JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE, 404),
        (JSON_RPC_INVALID_PARAMS_ERROR_CODE, 400),
        (JSON_RPC_PARSE_ERROR_CODE, 400),
        (JSON_RPC_INVALID_REQUEST_ERROR_CODE, 400),
        (CONTROL_OPERATION_REFUSED_ERROR_CODE, 422),
        (CONTROL_OPERATION_FAILED_ERROR_CODE, 500),
        (CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE, 409),
    ] {
        assert_eq!(
            the_http_status_of_a_control_error(code, None),
            status,
            "{code}"
        );
    }
}

#[test]
fn a_refusal_answers_the_status_its_refusal_code_gives_whatever_its_code() {
    for (code, refusal_code, status) in [
        (CONTROL_OPERATION_FAILED_ERROR_CODE, "stream_not_found", 404),
        (CONTROL_OPERATION_FAILED_ERROR_CODE, "node_not_found", 404),
        (CONTROL_OPERATION_REFUSED_ERROR_CODE, "node_not_found", 404),
        (CONTROL_OPERATION_FAILED_ERROR_CODE, "node_name_taken", 409),
        (CONTROL_OPERATION_FAILED_ERROR_CODE, "refused", 422),
        (CONTROL_OPERATION_FAILED_ERROR_CODE, "runtime_failed", 500),
        (
            JSON_RPC_INVALID_PARAMS_ERROR_CODE,
            "params_do_not_match_schema",
            400,
        ),
        (
            JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE,
            "operation_not_found",
            404,
        ),
    ] {
        assert_eq!(
            the_http_status_of_a_control_error(code, Some(refusal_code)),
            status,
            "error {code} refused as `{refusal_code}`"
        );
    }
}

#[test]
fn every_refusal_code_the_contract_lists_is_mapped_to_a_status_by_name() {
    let refusal_codes = the_refusal_codes_the_contract_lists(&control_contract_document());
    assert!(refusal_codes.iter().any(|listed| listed == "refused"));
    for refusal_code in refusal_codes {
        assert!(
            the_http_status_mapped_to_a_refusal_code(&refusal_code).is_some(),
            "`{refusal_code}` answers a status the mapping names, not its code's"
        );
    }
}

#[test]
fn every_error_code_the_contract_states_is_mapped_to_a_status_by_name() {
    let contract = control_contract_document();
    let stated_codes = contract["methods"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|method| method["errors"].as_array().into_iter().flatten())
        .map(|error| error["code"].as_i64().expect("a whole-number code"))
        .chain([
            JSON_RPC_PARSE_ERROR_CODE,
            JSON_RPC_INVALID_REQUEST_ERROR_CODE,
        ]);
    for code in stated_codes {
        assert!(
            the_http_status_mapped_to_a_control_error(code).is_some(),
            "error {code} answers a status the mapping names, not its fallback"
        );
    }
}

#[test]
fn each_operation_the_table_serves_only_on_a_connection_that_stays_open_is_marked_so() {
    let document = control_rest_openapi_document();
    let mut marked = 0;
    for declaration in CONTROL_OPERATIONS {
        let operation = the_operation_at(&document, declaration.method_name);
        let is_marked = operation["x-needs-a-connection"] == json!(true);
        assert_eq!(
            is_marked, declaration.needs_a_connection_that_stays_open,
            "`{}`",
            declaration.method_name
        );
        if is_marked {
            marked += 1;
            assert!(
                operation["description"].as_str().is_some_and(
                    |described| described.contains(CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET)
                ),
                "`{}`: {operation}",
                declaration.method_name
            );
        }
    }
    assert!(
        marked > 0,
        "the table names the operations that need a connection"
    );
}

#[test]
fn each_error_a_contract_method_states_is_described_at_each_status_it_may_answer() {
    let contract = control_contract_document();
    let document = the_rest_description_of(&contract);
    let refusal_statuses = the_statuses_a_refusal_may_answer(&contract);
    assert!(refusal_statuses.contains(&404) && refusal_statuses.contains(&500));
    for method in contract["methods"].as_array().into_iter().flatten() {
        let method_name = method["name"].as_str().expect("a method name");
        let responses = &the_operation_at(&document, method_name)["responses"];
        for error in method["errors"].as_array().into_iter().flatten() {
            let code = error["code"].as_i64().expect("a whole-number code");
            for status in the_statuses_an_error_may_answer(error, &refusal_statuses) {
                let schema = &responses[status.to_string()]["content"][JSON_MEDIA_TYPE]["schema"];
                let error_schemas = match schema.get("oneOf") {
                    Some(Value::Array(several)) => several.clone(),
                    _ => vec![schema.clone()],
                };
                assert!(
                    error_schemas.iter().any(|error_schema| {
                        error_schema["properties"]["code"]["const"] == code
                            && error_schema["properties"]["data"] == error["data"]
                    }),
                    "`{method_name}` describes its error {code} at {status}: {responses}"
                );
            }
        }
    }
}

#[test]
fn every_reference_in_the_description_resolves() {
    let document = control_rest_openapi_document();
    let mut references = Vec::new();
    every_reference_in(&document, &mut references);
    assert!(!references.is_empty());
    for reference in references {
        let pointer = reference
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("`{reference}` points outside the description"));
        assert!(
            document.pointer(pointer).is_some(),
            "`{reference}` resolves in the description"
        );
    }
}

fn every_reference_in(value: &Value, references: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                match (key.as_str(), field) {
                    ("$ref", Value::String(reference)) => references.push(reference.clone()),
                    _ => every_reference_in(field, references),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                every_reference_in(item, references);
            }
        }
        _ => {}
    }
}
