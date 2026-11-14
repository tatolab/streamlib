// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The contract file: the registry rendered as an OpenRPC document, its named types under
//! `components.schemas`.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectActionRefusalOnTheWire;

use crate::agent_surface::{AGENT_DISCOVERY_TOOLS, AgentDiscoveryToolDeclaration};
use crate::control_notification::{CONTROL_NOTIFICATIONS, ControlNotificationDeclaration};
use crate::control_operation::{ControlOperation, ControlOperationDeclaration};
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
