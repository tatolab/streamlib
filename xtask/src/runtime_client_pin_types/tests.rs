// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::contract_pin_types::component_schemas_of;

fn committed_contract() -> Value {
    serde_json::from_str(include_str!(
        "../../../runtime/streamlib-control-contract/contract.json"
    ))
    .unwrap()
}

#[test]
fn the_declarations_name_every_built_in_type_and_the_value_of_each_that_stores_one() {
    let contract = committed_contract();
    let pin_types =
        RuntimeClientPinTypes::of(&contract, component_schemas_of(&contract).unwrap()).unwrap();
    let declarations = pin_types.typescript_declarations();
    assert!(declarations.contains("export type BuiltInPinTypeName = \"boolean\" | \"integer\""));
    assert!(declarations.contains("  readonly color: ColorPinValue;\n"));
    assert!(declarations.contains("  readonly pin_type_choice: PinType;\n"));
    assert!(!declarations.contains("  readonly texture:"));
    for component in [
        "PinType",
        "DeclaredPinType",
        "PinFlow",
        "PinAnnotations",
        "PinDeclaration",
        "TexturePixelFormat",
        "BundleMember",
    ] {
        assert!(
            declarations.contains(&format!("export type {component} = "))
                || declarations.contains(&format!("export interface {component} ")),
            "{component}"
        );
    }
    assert!(!declarations.contains("import "));
}

#[test]
fn the_module_lists_each_row_frozen_with_its_value_component_only_where_it_has_one() {
    let contract = committed_contract();
    let pin_types =
        RuntimeClientPinTypes::of(&contract, component_schemas_of(&contract).unwrap()).unwrap();
    let module = pin_types.javascript();
    assert!(
        module.contains("  Object.freeze({\n    name: \"boolean\",\n    label: \"Boolean\",\n")
    );
    assert!(module.contains("    value_component: \"BooleanPinValue\",\n"));
    assert_eq!(
        module.matches("    value_component: ").count(),
        pin_types
            .rows
            .iter()
            .filter(|row| row.value_component.is_some())
            .count()
    );
    assert!(!module.contains("import "));
}

#[test]
fn a_hand_edit_of_the_pin_types_fails_the_check() {
    use crate::generate_runtime_client::{RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE, run};
    use streamlib_control_contract::CONTROL_CONTRACT_FILE_WORKSPACE_PATH;
    let workspace = tempfile::tempdir().unwrap();
    let contract_file_path = workspace.path().join(CONTROL_CONTRACT_FILE_WORKSPACE_PATH);
    std::fs::create_dir_all(contract_file_path.parent().unwrap()).unwrap();
    std::fs::write(
        &contract_file_path,
        include_str!("../../../runtime/streamlib-control-contract/contract.json"),
    )
    .unwrap();
    let package_directory = workspace
        .path()
        .join(RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE);
    std::fs::create_dir_all(&package_directory).unwrap();
    run(workspace.path(), false).unwrap();
    run(workspace.path(), true).unwrap();
    let pin_types_path = package_directory.join("pin_types.d.ts");
    let edited = std::fs::read_to_string(&pin_types_path)
        .unwrap()
        .replace("\"boolean\" | ", "");
    std::fs::write(&pin_types_path, edited).unwrap();
    let refusal = run(workspace.path(), true).err().unwrap().to_string();
    assert!(refusal.contains("pin_types.d.ts"), "{refusal}");
}
