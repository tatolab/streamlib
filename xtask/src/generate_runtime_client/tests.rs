// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

#[test]
fn a_schema_object_becomes_an_interface_with_its_optional_and_nullable_fields() {
    let schema = json!({
        "type": "object",
        "properties": {
            "stream": { "type": "string", "format": "uuid", "description": "The stream." },
            "name": { "type": ["string", "null"] },
            "pins": { "type": ["object", "null"], "additionalProperties": true }
        },
        "required": ["stream"],
        "additionalProperties": false
    });
    assert_eq!(
        object_type_of(&schema, 0),
        "{\n  /**\n   * The stream.\n   */\n  stream: string;\n  name?: string | null;\n  pins?: { [key: string]: unknown } | null;\n}"
    );
}

#[test]
fn a_tagged_union_a_reference_and_a_fixed_length_array_keep_their_shape() {
    assert_eq!(
        typescript_type_of(
            &json!({ "oneOf": [
                { "type": "object", "properties": { "operation": { "const": "add_node", "type": "string" } }, "required": ["operation"] },
                { "$ref": "#/components/schemas/StreamGraphActionLinkEnd" }
            ] }),
            0
        ),
        "{\n  operation: \"add_node\";\n} | StreamGraphActionLinkEnd"
    );
    assert_eq!(
        typescript_type_of(
            &json!({ "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 }),
            0
        ),
        "[number, number]"
    );
    assert_eq!(
        typescript_type_of(
            &json!({ "type": "object", "additionalProperties": false }),
            0
        ),
        "Record<string, never>"
    );
}

#[test]
fn the_committed_contract_file_generates_every_method_and_notification() {
    let contract_file_contents =
        include_str!("../../../runtime/streamlib-control-contract/contract.json");
    let contract: Value = serde_json::from_str(contract_file_contents).unwrap();
    let generated_files =
        runtime_client_generated_files(&contract, contract_file_contents).unwrap();
    let declarations = &generated_files[0].1;
    for method in contract["methods"].as_array().unwrap() {
        let method_name = method["name"].as_str().unwrap();
        assert!(
            declarations.contains(&format!("  {method_name}: {{\n")),
            "{method_name}"
        );
    }
    assert!(declarations.contains("  project_changed: ProjectChangedNotificationParams;"));
}

#[test]
fn the_client_has_one_method_per_operation_and_one_listener_per_notification() {
    let contract_file_contents =
        include_str!("../../../runtime/streamlib-control-contract/contract.json");
    let contract: Value = serde_json::from_str(contract_file_contents).unwrap();
    let generated_files =
        runtime_client_generated_files(&contract, contract_file_contents).unwrap();
    let client_source = &generated_files
        .iter()
        .find(|(file_name, _)| *file_name == GENERATED_CLIENT_FILE_NAME)
        .unwrap()
        .1;
    assert!(client_source.contains(
        "  open_project(params, callOptions) {\n    return this.call(\"open_project\", params, callOptions);\n  }"
    ));
    assert!(client_source.contains("  describe_runtime(params = {}, callOptions) {"));
    assert!(client_source.contains(
        "  on_project_changed(listener) {\n    return this.onNotification(\"project_changed\", listener);\n  }"
    ));
}

#[test]
fn a_property_defaulting_to_a_member_of_a_set_that_may_grow_reads_as_present() {
    let contract = json!({
        "info": { "x-contract-version": 1 },
        "components": { "schemas": {
            "Code": {
                "oneOf": [{ "type": "string", "const": "known" }, { "type": "string", "const": "refused" }],
                "x-may-grow": true
            },
            "Closed": { "enum": ["a", "b"] },
            "NodeUiManifest": { "type": "object" },
            "Refusal": {
                "type": "object",
                "properties": {
                    "code": { "$ref": "#/components/schemas/Code", "default": "refused" },
                    "closed": { "$ref": "#/components/schemas/Closed", "default": "a" },
                    "words": { "type": "string" }
                },
                "required": ["words"]
            }
        } },
        "methods": [],
        "x-notifications": []
    });
    let generated_files = runtime_client_generated_files(&contract, "").unwrap();
    let file = |name: &str| {
        &generated_files
            .iter()
            .find(|(file_name, _)| *file_name == name)
            .unwrap()
            .1
    };
    let schemas = file(GENERATED_SCHEMAS_FILE_NAME);
    assert!(
        schemas.contains("  code: Code.default(\"refused\"),"),
        "{schemas}"
    );
    assert!(
        schemas.contains("  closed: Closed.optional(),"),
        "{schemas}"
    );
    let types = file(GENERATED_CONTRACT_TYPES_FILE_NAME);
    assert!(types.contains("  code: Code;"), "{types}");
    assert!(types.contains("  closed?: Closed;"), "{types}");
}

#[test]
fn an_operation_named_like_a_member_of_the_client_is_refused() {
    let contract = json!({
        "info": { "x-contract-version": 1 },
        "components": { "schemas": {} },
        "methods": [{ "name": "close", "params": [{ "schema": {} }] }],
        "x-notifications": []
    });
    let refusal = runtime_client_generated_files(&contract, "")
        .err()
        .unwrap()
        .to_string();
    assert!(
        refusal.contains("`close` is the name of a member"),
        "{refusal}"
    );
}
