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
    assert_eq!(rows[0].name, "boolean");
    assert_eq!(rows[0].value_component, Some("BooleanPinValue"));
    assert!(rows.iter().any(|row| row.value_component.is_none()));
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
fn the_components_reached_from_a_root_follow_every_reference_in_the_file_order() {
    let contract = a_contract_with_rows(json!([]));
    let component_schemas = component_schemas_of(&contract).unwrap();
    assert_eq!(
        components_reached_from(component_schemas, &["BooleanPinValue", "PinType"]).unwrap(),
        ["PinType", "BooleanPinValue"]
    );
}
