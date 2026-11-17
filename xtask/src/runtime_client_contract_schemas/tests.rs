// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

fn refusal_of_components(component_schemas: Value) -> Option<String> {
    refuse_a_schema_the_checker_does_not_know(component_schemas.as_object().unwrap(), &[], &[])
        .err()
        .map(|refusal| refusal.to_string())
}

#[test]
fn a_keyword_the_checker_does_not_implement_stops_generation_naming_where_it_is() {
    let refusal = refusal_of_components(json!({
        "Closed": { "type": "object", "properties": { "kind": { "enum": ["a", "b"] } } }
    }))
    .unwrap();
    assert!(
        refusal.contains("components.schemas.Closed.properties.kind states `enum`"),
        "{refusal}"
    );
    let refusal =
        refusal_of_components(json!({ "Both": { "allOf": [{ "type": "string" }] } })).unwrap();
    assert!(refusal.contains("states `allOf`"), "{refusal}");
}

#[test]
fn a_type_or_format_the_checker_does_not_know_stops_generation() {
    assert!(
        refusal_of_components(json!({ "When": { "type": "string", "format": "date-time" } }))
            .unwrap()
            .contains("the format \"date-time\"")
    );
    assert!(
        refusal_of_components(json!({ "Odd": { "type": ["string", "decimal"] } }))
            .unwrap()
            .contains("the type \"decimal\"")
    );
}

#[test]
fn a_reference_to_no_component_and_a_set_that_cannot_fall_back_are_refused() {
    assert!(
        refusal_of_components(json!({ "Lost": { "$ref": "#/components/schemas/Nowhere" } }))
            .unwrap()
            .contains("no component of the contract")
    );
    assert!(
        refusal_of_components(json!({ "Grows": { "type": "string", "x-may-grow": true } }))
            .unwrap()
            .contains("x-may-grow")
    );
}

#[test]
fn a_property_named_like_a_keyword_is_a_name_not_a_keyword() {
    assert_eq!(
        refusal_of_components(json!({
            "Setting": {
                "type": "object",
                "properties": { "enum": { "type": "string" }, "default": { "type": "integer", "format": "uint32", "minimum": 0 } },
                "required": ["enum"],
                "additionalProperties": false
            }
        })),
        None
    );
}

#[test]
fn the_data_drops_descriptions_and_every_default_the_client_does_not_read() {
    let component_schemas = json!({
        "Refusal": {
            "type": "object",
            "description": "A refusal.",
            "properties": {
                "code": { "$ref": "#/components/schemas/Code", "default": "refused", "x-generated-reads-its-default": true, "description": "Its code." },
                "words": { "type": ["string", "null"], "default": null },
                "description": { "type": "string", "description": "A field named description." }
            }
        },
        "Code": { "oneOf": [{ "type": "string", "const": "refused", "description": "Refused." }], "x-may-grow": true }
    });
    let javascript = contract_schemas_javascript(
        component_schemas.as_object().unwrap(),
        &[ContractSchemasOfOneEntry {
            name: "graph",
            params_schema: &json!({ "$ref": "#/components/schemas/Refusal" }),
            result_schema: None,
        }],
        &[],
    );
    assert!(javascript.contains(
        "    Refusal: {\"type\":\"object\",\"properties\":{\"code\":{\"$ref\":\"#/components/schemas/Code\",\"default\":\"refused\",\"x-generated-reads-its-default\":true},\"words\":{\"type\":[\"string\",\"null\"]},\"description\":{\"type\":\"string\"}}},\n"
    ), "{javascript}");
    assert!(
        javascript.contains("    Code: {\"oneOf\":[{\"type\":\"string\",\"const\":\"refused\"}],\"x-may-grow\":true},\n"),
        "{javascript}"
    );
    assert!(
        javascript.contains(
            "    graph: { params: {\"$ref\":\"#/components/schemas/Refusal\"}, result: true },\n"
        ),
        "{javascript}"
    );
    assert!(
        !javascript.lines().any(|line| line.starts_with("import ")
            || line.starts_with("export ") && line.contains(" from ")),
        "{javascript}"
    );
}
