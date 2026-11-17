// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::contract_component_references::component_names_a_schema_names;

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
    let named = component_names_a_schema_names(&schema);
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

#[test]
fn a_hand_edit_of_a_node_ui_manifest_file_fails_the_check_naming_it() {
    use crate::generate_runtime_client::{
        RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE, run, runtime_client_generated_files,
    };
    use streamlib_control_contract::CONTROL_CONTRACT_FILE_WORKSPACE_PATH;

    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| panic!("xtask sits in the workspace"));
    let copied_workspace = tempfile::tempdir().unwrap_or_else(|why| panic!("{why}"));
    let copied_contract_file = copied_workspace
        .path()
        .join(CONTROL_CONTRACT_FILE_WORKSPACE_PATH);
    let copied_package = copied_workspace
        .path()
        .join(RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE);
    for directory in [
        copied_contract_file
            .parent()
            .unwrap_or_else(|| panic!("the contract file sits in a folder")),
        copied_package.as_path(),
    ] {
        std::fs::create_dir_all(directory).unwrap_or_else(|why| panic!("{why}"));
    }
    let contract_file_contents =
        std::fs::read_to_string(workspace_root.join(CONTROL_CONTRACT_FILE_WORKSPACE_PATH))
            .unwrap_or_else(|why| panic!("{why}"));
    std::fs::write(&copied_contract_file, &contract_file_contents)
        .unwrap_or_else(|why| panic!("{why}"));
    let contract: Value =
        serde_json::from_str(&contract_file_contents).unwrap_or_else(|why| panic!("{why}"));
    let generated_files = runtime_client_generated_files(&contract, &contract_file_contents)
        .unwrap_or_else(|why| panic!("{why}"));
    for (file_name, _) in &generated_files {
        std::fs::copy(
            workspace_root
                .join(RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE)
                .join(file_name),
            copied_package.join(file_name),
        )
        .unwrap_or_else(|why| panic!("copying {file_name}: {why}"));
    }
    run(copied_workspace.path(), true)
        .unwrap_or_else(|why| panic!("the unedited copy passes the check: {why}"));

    for file_name in [
        NODE_UI_MANIFEST_JSON_SCHEMA_FILE_NAME,
        NODE_UI_MANIFEST_TYPES_FILE_NAME,
    ] {
        let file_path = copied_package.join(file_name);
        let as_generated =
            std::fs::read_to_string(&file_path).unwrap_or_else(|why| panic!("{why}"));
        std::fs::write(&file_path, format!("{as_generated}// edited by hand\n"))
            .unwrap_or_else(|why| panic!("{why}"));
        let refusal = run(copied_workspace.path(), true)
            .err()
            .unwrap_or_else(|| panic!("a hand edit of {file_name} fails the check"))
            .to_string();
        assert!(refusal.contains(file_name), "{refusal}");
        std::fs::write(&file_path, as_generated).unwrap_or_else(|why| panic!("{why}"));
    }
}
