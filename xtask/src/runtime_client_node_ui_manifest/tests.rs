// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn component_schemas() -> Map<String, Value> {
    let contract: Value = serde_json::from_str(include_str!(
        "../../../runtime/streamlib-control-contract/contract.json"
    ))
    .unwrap_or_else(|why| panic!("the contract file parses: {why}"));
    contract["components"]["schemas"]
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("the contract file has component schemas"))
}

#[test]
fn the_json_schema_holds_every_definition_it_names_and_no_contract_reference() {
    let schema: Value = serde_json::from_str(
        &node_ui_manifest_json_schema(&component_schemas()).unwrap_or_else(|why| panic!("{why}")),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(schema["$schema"], json!(JSON_SCHEMA_DIALECT));
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(
        schema["properties"]["controls"]["items"],
        json!({ "$ref": "#/$defs/NodeUiManifestControl" })
    );
    assert!(schema["description"].is_string());
    let mut named = Vec::new();
    every_component_named_in(&schema, &mut named);
    assert!(named.is_empty(), "{named:?}");
    let written = schema.to_string();
    for reference in written.split("\"#/$defs/").skip(1) {
        let name = reference.split('"').next().unwrap_or_default();
        assert!(
            schema["$defs"].get(name).is_some(),
            "`{name}` is not defined"
        );
    }
    assert_eq!(
        schema["$defs"]["NodeUiManifestControl"]["properties"]["type"]["$ref"],
        json!("#/$defs/BuiltInPinTypeName")
    );
    assert!(
        schema["$defs"]["BuiltInPinTypeName"]["oneOf"][0]["description"].is_string(),
        "the pin type names keep their descriptions"
    );
}

#[test]
fn the_declarations_name_the_manifest_and_every_type_it_names() {
    let declarations =
        node_ui_manifest_declarations(&component_schemas()).unwrap_or_else(|why| panic!("{why}"));
    for name in [
        "BuiltInPinTypeName",
        "NodeUiManifest",
        "NodeUiManifestControl",
        "NodeUiManifestNodeBody",
    ] {
        assert!(declarations.contains(&format!("  {name},\n")), "{name}");
    }
    assert!(declarations.ends_with("} from \"./control_contract.js\";\n"));
}

#[test]
fn a_contract_without_the_manifest_is_refused() {
    let refusal = node_ui_manifest_json_schema(&Map::new())
        .err()
        .unwrap_or_else(|| panic!("a contract without the manifest is refused"))
        .to_string();
    assert!(refusal.contains("`NodeUiManifest`"), "{refusal}");
}
