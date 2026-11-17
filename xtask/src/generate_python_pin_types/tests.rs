// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

fn committed_contract() -> Value {
    serde_json::from_str(include_str!(
        "../../../runtime/streamlib-control-contract/contract.json"
    ))
    .unwrap()
}

#[test]
fn the_checked_in_module_is_what_the_committed_contract_file_generates() {
    assert_eq!(
        render_python_pin_types_module(&committed_contract()).unwrap(),
        include_str!("../../../sdk/tatolab-stream/tatolab/stream/_contract_pin_types.py")
    );
}

#[test]
fn the_module_names_every_built_in_type_and_the_wire_form_of_a_declaration() {
    let module = render_python_pin_types_module(&committed_contract()).unwrap();
    assert!(
        module.contains("type BuiltInPinTypeName = Literal[\n    \"boolean\",\n    \"integer\",\n")
    );
    assert!(module.contains(
        "type NodeInputMode = Literal[\"wire_or_value\", \"wire_only\", \"setting_only\"]"
    ));
    assert!(module.contains("type PinFlow = Literal[\"value\", \"stream\", \"trigger\"]"));
    assert!(module.contains("class PinDeclarationOnTheWire(TypedDict):"));
    assert!(module.contains("    pin_type: DeclaredPinType\n"));
    assert!(module.contains("class ListOfPinType(TypedDict):"));
    assert!(module.contains("    element_type: PinType\n"));
    assert!(module.contains("type PinType = (\n    BooleanPinType\n    | IntegerPinType\n"));
    assert!(module.contains("    media: NotRequired[list[ProjectAssetMedia]]\n"));
    assert!(module.contains("        python_forms=(\"Vector2\", \"Vector3\", \"Vector4\"),\n"));
    assert!(!module.contains("class PinDeclaration(TypedDict)"));
}

#[test]
fn a_hand_edit_of_the_module_fails_the_check() {
    let workspace = tempfile::tempdir().unwrap();
    let contract_file_path = workspace.path().join(CONTROL_CONTRACT_FILE_WORKSPACE_PATH);
    std::fs::create_dir_all(contract_file_path.parent().unwrap()).unwrap();
    std::fs::write(
        &contract_file_path,
        include_str!("../../../runtime/streamlib-control-contract/contract.json"),
    )
    .unwrap();
    let module_path = workspace.path().join(GENERATED_PYTHON_PIN_TYPES_PATH);
    std::fs::create_dir_all(module_path.parent().unwrap()).unwrap();
    run(workspace.path(), false).unwrap();
    run(workspace.path(), true).unwrap();
    let edited = std::fs::read_to_string(&module_path)
        .unwrap()
        .replace("\"boolean\"", "\"bool\"");
    std::fs::write(&module_path, edited).unwrap();
    let refusal = run(workspace.path(), true).err().unwrap().to_string();
    assert!(refusal.contains("generate-python-pin-types"), "{refusal}");
}

#[test]
fn a_key_python_cannot_declare_in_a_class_is_refused() {
    let mut contract = committed_contract();
    contract["components"]["schemas"]["PinNumberRange"]["properties"]["from"] =
        json!({ "type": "number" });
    let refusal = render_python_pin_types_module(&contract)
        .err()
        .unwrap()
        .to_string();
    assert!(refusal.contains("`from`"), "{refusal}");
}
