// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

fn a_contract_with_rows(rows: Value) -> Value {
    json!({
        "x-pin-types": rows,
        "components": { "schemas": {
            "PinType": { "oneOf": [
                { "type": "object", "properties": { "type": { "const": "boolean" } } },
                { "type": "object", "properties": {
                    "type": { "const": "list_of" },
                    "element_type": { "$ref": "#/components/schemas/PinType" }
                } }
            ] },
            "BuiltInPinTypeName": { "oneOf": [{ "const": "boolean" }, { "const": "list_of" }] },
            "PinAnnotations": { "type": "object", "properties": { "meaning": { "type": "string" } } },
            "BooleanPinValue": { "type": "boolean" },
            "Unreached": { "type": "string" }
        } }
    })
}

fn a_row(name: &str, annotations: Value) -> Value {
    json!({
        "name": name, "label": name, "documentation": "", "flows": ["value"],
        "annotations": annotations, "python_forms": [], "typescript_form": format!("\"{name}\""),
        "x-since": 3, "x-stability": "experimental"
    })
}

#[test]
fn the_committed_contract_file_has_a_row_for_every_pin_type_alternative() {
    let contract: Value = serde_json::from_str(include_str!(
        "../../../runtime/streamlib-control-contract/contract.json"
    ))
    .unwrap();
    let rows = built_in_pin_type_rows(&contract).unwrap();
    let schemas = &contract["components"]["schemas"];
    let row_names: Vec<&str> = rows.iter().map(|row| row.name).collect();
    let tags: Vec<&str> = schemas["PinType"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alternative| alternative["properties"]["type"]["const"].as_str().unwrap())
        .collect();
    assert_eq!(row_names, tags);
    let value_of = |name: &str| {
        rows.iter()
            .find(|row| row.name == name)
            .unwrap()
            .value_component
    };
    assert_eq!(value_of("boolean"), Some("BooleanPinValue"));
    assert_eq!(value_of("asset"), Some("AssetPinValue"));
    assert_eq!(value_of("pin_type_choice"), Some("PinType"));
    assert_eq!(value_of("texture"), None);
    assert_eq!(value_of("trigger"), None);
    for row in &rows {
        assert_eq!(row.typescript_form, format!("\"{}\"", row.name));
        assert!(
            !row.flows.is_empty() && !row.python_forms.is_empty(),
            "{}",
            row.name
        );
        if let Some(value_component) = row.value_component {
            assert!(schemas.get(value_component).is_some(), "{value_component}");
        }
    }
}

#[test]
fn rows_that_differ_from_the_pin_type_alternatives_are_refused() {
    let refusal =
        built_in_pin_type_rows(&a_contract_with_rows(json!([a_row("boolean", json!([]))])))
            .err()
            .unwrap()
            .to_string();
    assert!(refusal.contains("must name the same types"), "{refusal}");
}

#[test]
fn a_row_naming_an_annotation_pin_annotations_lacks_is_refused() {
    let refusal = built_in_pin_type_rows(&a_contract_with_rows(json!([
        a_row("boolean", json!(["meaning"])),
        a_row("list_of", json!(["range"]))
    ])))
    .err()
    .unwrap()
    .to_string();
    assert!(refusal.contains("`range`"), "{refusal}");
}

#[test]
fn rows_that_differ_from_the_built_in_pin_type_names_are_refused() {
    let mut contract = a_contract_with_rows(json!([
        a_row("boolean", json!([])),
        a_row("list_of", json!([]))
    ]));
    contract["components"]["schemas"]["BuiltInPinTypeName"] =
        json!({ "oneOf": [{ "const": "list_of" }, { "const": "boolean" }] });
    let refusal = built_in_pin_type_rows(&contract).err().unwrap().to_string();
    assert!(refusal.contains("`BuiltInPinTypeName` holds"), "{refusal}");
}

#[test]
fn a_value_schema_naming_no_component_is_refused() {
    let mut boolean = a_row("boolean", json!([]));
    boolean["value_json_schema"] = json!({ "$ref": "#/components/schemas/NoSuchPinValue" });
    let refusal = built_in_pin_type_rows(&a_contract_with_rows(json!([
        boolean,
        a_row("list_of", json!([]))
    ])))
    .err()
    .unwrap()
    .to_string();
    assert!(
        refusal.contains("`NoSuchPinValue`, which is not a component"),
        "{refusal}"
    );
}

#[test]
fn a_value_schema_that_is_not_a_reference_is_refused() {
    let mut boolean = a_row("boolean", json!([]));
    boolean["value_json_schema"] = json!({ "type": "boolean" });
    let refusal = built_in_pin_type_rows(&a_contract_with_rows(json!([
        boolean,
        a_row("list_of", json!([]))
    ])))
    .err()
    .unwrap()
    .to_string();
    assert!(refusal.contains("not a component reference"), "{refusal}");
}

#[test]
fn a_stability_other_than_stable_or_experimental_is_refused() {
    let mut boolean = a_row("boolean", json!([]));
    boolean["x-stability"] = json!("deprecated");
    let refusal = built_in_pin_type_rows(&a_contract_with_rows(json!([
        boolean,
        a_row("list_of", json!([]))
    ])))
    .err()
    .unwrap()
    .to_string();
    assert!(refusal.contains("`deprecated`"), "{refusal}");
}
