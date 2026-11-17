// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;

fn component_schemas(schemas: Value) -> Map<String, Value> {
    schemas
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("the schemas are an object"))
}

#[test]
fn a_reference_inside_a_const_or_a_default_names_nothing() {
    let schema = json!({
        "properties": {
            "chosen": { "$ref": "#/components/schemas/PinType" },
            "kept": { "const": { "$ref": "#/components/schemas/NotNamed" } },
            "given": {
                "default": [{ "$ref": "#/components/schemas/NotNamedEither" }],
                "anyOf": [{ "$ref": "#/components/schemas/PinFlow" }, { "$ref": "#/$defs/Local" }]
            }
        }
    });
    assert_eq!(
        component_names_a_schema_names(&schema),
        vec!["PinType", "PinFlow"]
    );
}

#[test]
fn every_component_a_root_reaches_is_listed_once_in_the_contracts_order() {
    let schemas = component_schemas(json!({
        "Unreached": { "$ref": "#/components/schemas/Leaf" },
        "Leaf": { "type": "string" },
        "Middle": { "items": { "$ref": "#/components/schemas/Leaf" } },
        "Root": {
            "properties": {
                "middle": { "$ref": "#/components/schemas/Middle" },
                "again": { "$ref": "#/components/schemas/Root" }
            }
        },
        "OtherRoot": { "$ref": "#/components/schemas/Leaf" }
    }));
    let reached = component_names_reached_from(&schemas, &["Root", "OtherRoot"])
        .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(reached, vec!["Leaf", "Middle", "Root", "OtherRoot"]);
}

#[test]
fn a_root_or_a_reference_the_contract_has_no_schema_for_is_refused() {
    let schemas = component_schemas(json!({
        "Root": { "$ref": "#/components/schemas/Missing" }
    }));
    let refusal = component_names_reached_from(&schemas, &["Absent"])
        .err()
        .unwrap_or_else(|| panic!("an absent root is refused"))
        .to_string();
    assert!(refusal.contains("`Absent`"), "{refusal}");
    let refusal = component_names_reached_from(&schemas, &["Root"])
        .err()
        .unwrap_or_else(|| panic!("a reference to an absent schema is refused"))
        .to_string();
    assert!(refusal.contains("`Root` names `Missing`"), "{refusal}");
}
