// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The contract file: the registry rendered as an OpenRPC document, its named types under
//! `components.schemas`.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};
use streamlib_node_schema::pin_type::{
    NodeInputMode, PinDeclaration, PinType, PinTypeDescription, PinTypeStability,
    built_in_pin_types,
};
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectActionRefusalOnTheWire;

use crate::agent_surface::{AGENT_DISCOVERY_TOOLS, AgentDiscoveryToolDeclaration};
use crate::control_notification::{CONTROL_NOTIFICATIONS, ControlNotificationDeclaration};
use crate::control_operation::{
    ControlOperation, ControlOperationDeclaration, ControlOperationStability,
};
use crate::control_operation_results::ControlContractVersionMismatch;
use crate::control_operations::{CONTROL_CONTRACT_VERSION, CONTROL_OPERATIONS, Discover};

/// The OpenRPC version the contract file is written in.
pub const OPENRPC_DOCUMENT_VERSION: &str = "1.3.2";

/// The path of the contract file in the repository, from the workspace root.
pub const CONTROL_CONTRACT_FILE_WORKSPACE_PATH: &str =
    "runtime/streamlib-control-contract/contract.json";

/// The JSON-RPC error code of an operation the runtime refused, its `data` the refusal.
pub const CONTROL_OPERATION_REFUSED_ERROR_CODE: i64 = 1;

/// The JSON-RPC error code of an operation that could not be carried out, its message the
/// runtime's own words.
pub const CONTROL_OPERATION_FAILED_ERROR_CODE: i64 = 2;

/// The JSON-RPC error code of a `discover` from a client built against another contract version,
/// its `data` both versions.
pub const CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE: i64 = 3;

const COMPONENT_SCHEMAS_DEFINITIONS_PATH: &str = "/components/schemas/";

/// The contract file's contents, as written to disk: pretty JSON ending in a newline.
pub fn control_contract_file_contents() -> String {
    let mut contents = serde_json::to_string_pretty(&control_contract_document())
        .unwrap_or_else(|_| unreachable!("a JSON value always serializes"));
    contents.push('\n');
    contents
}

/// The registry as one OpenRPC document.
pub fn control_contract_document() -> Value {
    let mut schema_generator = SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.definitions_path = COMPONENT_SCHEMAS_DEFINITIONS_PATH.into();
        })
        .into_generator();
    let refusal_schema = schema_generator.subschema_for::<ProjectActionRefusalOnTheWire>();
    let contract_version_mismatch_schema =
        schema_generator.subschema_for::<ControlContractVersionMismatch>();
    let methods: Vec<Value> = CONTROL_OPERATIONS
        .iter()
        .map(|declaration| {
            the_method_entry_of(
                declaration,
                &mut schema_generator,
                &refusal_schema,
                &contract_version_mismatch_schema,
            )
        })
        .collect();
    let notifications: Vec<Value> = CONTROL_NOTIFICATIONS
        .iter()
        .map(|declaration| the_notification_entry_of(declaration, &mut schema_generator))
        .collect();
    let agent_discovery_tools: Vec<Value> = AGENT_DISCOVERY_TOOLS
        .iter()
        .map(|declaration| the_agent_discovery_tool_entry_of(declaration, &mut schema_generator))
        .collect();
    let pin_types: Vec<Value> = built_in_pin_types()
        .iter()
        .map(|row| the_pin_type_entry_of(row, &mut schema_generator))
        .collect();
    the_pin_type_components_added_to(&mut schema_generator);
    let component_schemas: Map<String, Value> = schema_generator.take_definitions(true);
    json!({
        "openrpc": OPENRPC_DOCUMENT_VERSION,
        "info": {
            "title": "StreamLib runtime control API",
            "version": CONTROL_CONTRACT_VERSION.to_string(),
            "description": "Every operation a StreamLib runtime's control plane serves. `x-contract-version` is bumped only by a breaking change; an addition is marked by its method's `x-since`.",
            "x-contract-version": CONTROL_CONTRACT_VERSION,
        },
        "methods": methods,
        "x-notifications": notifications,
        "x-agent-discovery-tools": agent_discovery_tools,
        "x-pin-types": pin_types,
        "components": { "schemas": component_schemas },
    })
}

fn the_method_entry_of(
    declaration: &ControlOperationDeclaration,
    schema_generator: &mut schemars::SchemaGenerator,
    refusal_schema: &schemars::Schema,
    contract_version_mismatch_schema: &schemars::Schema,
) -> Value {
    let request_schema = (declaration.request_json_schema)(schema_generator);
    let result_schema = (declaration.result_json_schema)(schema_generator);
    json!({
        "name": declaration.method_name,
        "summary": declaration.one_line_doc,
        "description": declaration.agent_usage_note,
        "paramStructure": "by-name",
        "params": [{
            "name": "params",
            "required": true,
            "schema": request_schema,
        }],
        "result": {
            "name": format!("{}_result", declaration.method_name),
            "schema": result_schema,
        },
        "errors": the_errors_of(declaration, refusal_schema, contract_version_mismatch_schema),
        "x-since": declaration.since_contract_version,
        "x-stability": declaration.stability,
        "x-group": declaration.group,
        "x-exposures": declaration.exposures,
        "x-effect": declaration.effect,
        "x-agent-tool-listing": declaration.agent_tool_listing,
    })
}

fn the_agent_discovery_tool_entry_of(
    declaration: &AgentDiscoveryToolDeclaration,
    schema_generator: &mut schemars::SchemaGenerator,
) -> Value {
    let mut entry = json!({
        "name": declaration.tool_name,
        "summary": declaration.one_line_doc,
        "description": declaration.agent_usage_note,
        "params": (declaration.request_json_schema)(schema_generator),
        "x-effect": declaration.effect,
    });
    if let Some(result_json_schema) = declaration.result_json_schema {
        entry["result"] = result_json_schema(schema_generator).to_value();
    }
    entry
}

fn the_pin_type_entry_of(
    row: &PinTypeDescription,
    schema_generator: &mut schemars::SchemaGenerator,
) -> Value {
    let mut entry = json!({
        "name": row.name,
        "label": row.label,
        "documentation": row.documentation,
        "flows": row.flows,
        "annotations": row.annotations,
        "python_form": row.python_form,
        "typescript_form": row.typescript_form,
        "x-since": row.since_contract_version,
        "x-stability": the_contract_stability_of(row.stability),
    });
    if let Some(value_json_schema) = row.name.value_json_schema(schema_generator) {
        entry["value_json_schema"] = value_json_schema.to_value();
    }
    entry
}

fn the_contract_stability_of(stability: PinTypeStability) -> ControlOperationStability {
    match stability {
        PinTypeStability::Stable => ControlOperationStability::Stable,
        PinTypeStability::Experimental => ControlOperationStability::Experimental,
    }
}

/// Adds the pin type shapes no operation names yet, so a client generates them from the file.
fn the_pin_type_components_added_to(schema_generator: &mut schemars::SchemaGenerator) {
    schema_generator.subschema_for::<PinType>();
    schema_generator.subschema_for::<PinDeclaration>();
    schema_generator.subschema_for::<NodeInputMode>();
}

fn the_errors_of(
    declaration: &ControlOperationDeclaration,
    refusal_schema: &schemars::Schema,
    contract_version_mismatch_schema: &schemars::Schema,
) -> Vec<Value> {
    let mut errors = vec![
        json!({
            "code": CONTROL_OPERATION_REFUSED_ERROR_CODE,
            "message": "refused",
            "data": refusal_schema,
        }),
        json!({
            "code": CONTROL_OPERATION_FAILED_ERROR_CODE,
            "message": "failed",
        }),
    ];
    if declaration.method_name == Discover::METHOD_NAME {
        errors.push(json!({
            "code": CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE,
            "message": "contract version mismatch",
            "data": contract_version_mismatch_schema,
        }));
    }
    errors
}

fn the_notification_entry_of(
    declaration: &ControlNotificationDeclaration,
    schema_generator: &mut schemars::SchemaGenerator,
) -> Value {
    json!({
        "name": declaration.method_name,
        "summary": declaration.one_line_doc,
        "paramStructure": "by-name",
        "params": [{
            "name": "params",
            "required": true,
            "schema": (declaration.params_json_schema)(schema_generator),
        }],
        "x-subscribed-by": declaration.subscribed_by_method_name,
        "x-since": declaration.since_contract_version,
        "x-stability": declaration.stability,
        "x-exposures": declaration.exposures,
    })
}

#[cfg(test)]
mod tests {
    use streamlib_node_schema::pin_type::BuiltInPinTypeName;

    use super::*;

    #[test]
    fn the_document_lists_every_registered_operation_in_table_order() {
        let document = control_contract_document();
        let method_names: Vec<&str> = document["methods"]
            .as_array()
            .unwrap_or_else(|| panic!("`methods` is an array"))
            .iter()
            .map(|method| method["name"].as_str().unwrap_or_default())
            .collect();
        let registered: Vec<&str> = CONTROL_OPERATIONS
            .iter()
            .map(|declaration| declaration.method_name)
            .collect();
        assert_eq!(method_names, registered);
        assert_eq!(
            document["info"]["x-contract-version"],
            CONTROL_CONTRACT_VERSION
        );
    }

    #[test]
    fn every_reference_in_the_document_resolves_to_a_component_schema() {
        let document = control_contract_document();
        let component_schemas = document["components"]["schemas"]
            .as_object()
            .unwrap_or_else(|| panic!("`components.schemas` is an object"))
            .clone();
        let mut references = Vec::new();
        every_reference_in(&document, &mut references);
        assert!(!references.is_empty());
        for reference in references {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("`{reference}` points outside `components.schemas`"));
            assert!(
                component_schemas.contains_key(name),
                "`{reference}` resolves to nothing"
            );
        }
    }

    #[test]
    fn the_committed_contract_file_is_what_the_registry_generates() {
        let committed = include_str!("../contract.json");
        assert!(
            committed == control_contract_file_contents(),
            "contract.json differs from the registry; run `cargo xtask generate-control-contract`"
        );
    }

    #[test]
    fn the_pin_type_list_has_one_experimental_row_per_built_in_pin_type_in_order() {
        let document = control_contract_document();
        let rows = document["x-pin-types"]
            .as_array()
            .unwrap_or_else(|| panic!("`x-pin-types` is an array"));
        let row_names: Vec<Value> = rows.iter().map(|row| row["name"].clone()).collect();
        let built_in_names: Vec<Value> = BuiltInPinTypeName::EVERY_NAME
            .iter()
            .map(|name| json!(name))
            .collect();
        assert_eq!(row_names, built_in_names);
        for row in rows {
            assert_eq!(row["x-stability"], json!("experimental"), "{}", row["name"]);
            assert_eq!(
                row["x-since"],
                json!(CONTROL_CONTRACT_VERSION),
                "{}",
                row["name"]
            );
            for field in ["label", "documentation", "python_form", "typescript_form"] {
                assert!(row[field].is_string(), "{} has no `{field}`", row["name"]);
            }
            assert!(
                row["flows"]
                    .as_array()
                    .is_some_and(|flows| !flows.is_empty())
            );
            assert!(row["annotations"].is_array());
        }
    }

    #[test]
    fn a_pin_type_row_states_a_value_schema_exactly_when_its_type_stores_a_value() {
        let stores_no_value = [
            "texture",
            "audio",
            "encoded_video",
            "encoded_audio",
            "tensor",
            "trigger",
            "named",
        ];
        let document = control_contract_document();
        for row in document["x-pin-types"].as_array().into_iter().flatten() {
            let name = row["name"].as_str().unwrap_or_default();
            assert_eq!(
                row.get("value_json_schema").is_some(),
                !stores_no_value.contains(&name),
                "`{name}`"
            );
        }
        assert_eq!(
            document["x-pin-types"][BuiltInPinTypeName::Asset.position()]["value_json_schema"],
            json!({ "$ref": "#/components/schemas/AssetPinValue" })
        );
    }

    #[test]
    fn the_pin_type_shapes_are_component_schemas() {
        let document = control_contract_document();
        for name in [
            "PinType",
            "DeclaredPinType",
            "PinFlow",
            "PinAnnotations",
            "PinDeclaration",
            "NodeInputMode",
            "BundleMember",
        ] {
            assert!(
                document["components"]["schemas"].get(name).is_some(),
                "`{name}` is not a component schema"
            );
        }
        assert!(
            document["components"]["schemas"]["BundleMember"]["properties"]
                .get("default_value")
                .is_some()
        );
    }

    #[test]
    fn every_component_schema_uses_only_the_keywords_the_contract_uses() {
        const KEYWORDS: [&str; 15] = [
            "type",
            "properties",
            "required",
            "additionalProperties",
            "items",
            "minItems",
            "maxItems",
            "const",
            "oneOf",
            "anyOf",
            "$ref",
            "format",
            "minimum",
            "description",
            "default",
        ];
        let document = control_contract_document();
        let mut keywords = std::collections::BTreeSet::new();
        for schema in document["components"]["schemas"]
            .as_object()
            .into_iter()
            .flat_map(|schemas| schemas.values())
        {
            every_keyword_in(schema, &mut keywords);
        }
        let unknown: Vec<&String> = keywords
            .iter()
            .filter(|keyword| !KEYWORDS.contains(&keyword.as_str()))
            .collect();
        assert!(
            unknown.is_empty(),
            "keywords outside the contract's: {unknown:?}"
        );
    }

    fn every_keyword_in(schema: &Value, keywords: &mut std::collections::BTreeSet<String>) {
        match schema {
            Value::Object(object) => {
                for (keyword, inner) in object {
                    keywords.insert(keyword.clone());
                    match keyword.as_str() {
                        "properties" => inner
                            .as_object()
                            .into_iter()
                            .flat_map(|properties| properties.values())
                            .for_each(|property| every_keyword_in(property, keywords)),
                        "default" | "const" => {}
                        _ => every_keyword_in(inner, keywords),
                    }
                }
            }
            Value::Array(items) => items
                .iter()
                .for_each(|item| every_keyword_in(item, keywords)),
            _ => {}
        }
    }

    fn every_reference_in(value: &Value, references: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, inner) in object {
                    match (key.as_str(), inner) {
                        ("$ref", Value::String(reference)) => references.push(reference.clone()),
                        _ => every_reference_in(inner, references),
                    }
                }
            }
            Value::Array(items) => items
                .iter()
                .for_each(|item| every_reference_in(item, references)),
            _ => {}
        }
    }
}
