// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::control_operations::CONTROL_OPERATIONS;

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
        let method_name = method["name"].as_str().unwrap_or_default();
        let operation = the_operation_at(&document, method_name);
        assert_eq!(operation["operationId"], method["name"]);
        assert_eq!(
            operation["description"], method["description"],
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
fn each_error_a_contract_method_states_is_described_at_the_status_its_code_gives() {
    let contract = control_contract_document();
    let document = the_rest_description_of(&contract);
    for method in contract["methods"].as_array().into_iter().flatten() {
        let method_name = method["name"].as_str().unwrap_or_default();
        let responses = &the_operation_at(&document, method_name)["responses"];
        for error in method["errors"].as_array().into_iter().flatten() {
            let code = error["code"].as_i64().unwrap_or_default();
            let status = the_http_status_of_a_control_error(code);
            assert_ne!(
                status, 500,
                "`{method_name}`'s error {code} has a status of its own"
            );
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
