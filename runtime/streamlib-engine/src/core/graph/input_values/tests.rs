// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

use crate::core::test_support::{
    MockInputOnlyNode, MockPinTypedNode, MockSourceTakingOneSetting, ensure_test_mocks_registered,
};

/// The refusal, if any, of `value` set on input `input` of a node named `node` of `node_type`
/// holding no other input value.
fn the_refusal_of(
    node_type: &NodeClassImportPath,
    input: &str,
    value: Option<Value>,
) -> Result<(), InputValueRefusal> {
    ensure_test_mocks_registered();
    let mut input_values_after_the_set = Map::new();
    if let Some(value) = &value {
        input_values_after_the_set.insert(input.to_owned(), value.clone());
    }
    refuse_an_input_value_its_input_does_not_take(
        &NodeTypesOneStreamResolves::new(),
        &NodeAnInputValueIsSetOn {
            name: "mixer",
            node_type,
            input_values_after_the_set: &input_values_after_the_set,
        },
        input,
        value.as_ref(),
    )
}

fn the_code_of(refusal: Result<(), InputValueRefusal>) -> InputValueRefusalCode {
    refusal.expect_err("the value is refused").code
}

#[test]
fn a_typed_input_takes_what_its_pin_type_takes_and_refuses_the_rest() {
    let node_type = MockPinTypedNode::node_class_import_path();
    assert_eq!(the_refusal_of(&node_type, "gain", Some(json!(0.5))), Ok(()));
    assert_eq!(the_refusal_of(&node_type, "gain", None), Ok(()));
    let refusal =
        the_refusal_of(&node_type, "gain", Some(json!("loud"))).expect_err("text is no Float");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
    assert!(
        refusal.words.contains("`gain`") && refusal.words.contains("`mixer`"),
        "{refusal}"
    );
}

#[test]
fn a_name_that_is_no_input_and_no_setting_is_refused_as_not_found() {
    assert_eq!(
        the_code_of(the_refusal_of(
            &MockPinTypedNode::node_class_import_path(),
            "loudness",
            Some(json!(1))
        )),
        InputValueRefusalCode::PortNotFound
    );
    let refusal = the_refusal_of(
        &MockSourceTakingOneSetting::node_class_import_path(),
        "frame_height",
        Some(json!(480)),
    )
    .expect_err("the config schema lists no `frame_height`");
    assert_eq!(refusal.code, InputValueRefusalCode::PortNotFound);
    assert!(refusal.words.contains("`frame_width`"), "{refusal}");
}

#[test]
fn an_untyped_input_that_is_no_channels_input_takes_no_value() {
    assert_eq!(
        the_code_of(the_refusal_of(
            &MockInputOnlyNode::node_class_import_path(),
            "in1",
            Some(json!(1))
        )),
        InputValueRefusalCode::InputTakesNoValue
    );
}

#[test]
fn a_setting_is_checked_against_its_node_types_config_schema() {
    let node_type = MockSourceTakingOneSetting::node_class_import_path();
    assert_eq!(
        the_refusal_of(&node_type, "frame_width", Some(json!(640))),
        Ok(())
    );
    assert_eq!(the_refusal_of(&node_type, "frame_width", None), Ok(()));
    assert_eq!(
        the_code_of(the_refusal_of(
            &node_type,
            "frame_width",
            Some(json!("wide"))
        )),
        InputValueRefusalCode::InputValueNotValid
    );
}

#[test]
fn a_number_outside_its_declared_range_is_refused_and_never_clamped() {
    let range = PinNumberRange {
        minimum: Some(0.0),
        maximum: Some(1.0),
    };
    assert_eq!(
        refuse_a_number_outside_the_range("mixer", "gain", 0.5, range),
        Ok(())
    );
    let refusal = refuse_a_number_outside_the_range("mixer", "gain", 1.5, range)
        .expect_err("1.5 lies above the range");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
    assert!(refusal.words.contains("from 0 to 1"), "{refusal}");
}

#[test]
fn an_exposure_is_recorded_only_on_a_node_type_that_takes_a_config() {
    ensure_test_mocks_registered();
    let node_types = NodeTypesOneStreamResolves::new();
    let exposing = |node_type: &NodeClassImportPath, names: &[&str]| {
        an_exposure_of_config_fields_is_recorded_on(
            &node_types,
            node_type,
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert!(
        exposing(&MockSourceTakingOneSetting::node_class_import_path(), &[]).unwrap(),
        "a type taking a config records its exposures"
    );
    assert!(
        !exposing(&MockPinTypedNode::node_class_import_path(), &["gain"]).unwrap(),
        "a type taking no config records none"
    );
    let refusal = exposing(&MockPinTypedNode::node_class_import_path(), &["volume"])
        .expect_err("`volume` is none of its inputs");
    assert!(refusal.to_string().contains("`volume`"), "{refusal}");
}

/// A Channels input typed `pin_type`, named `label`.
fn a_channels_input_typed(pin_type: crate::core::descriptors::PinType) -> PortInfo {
    PortInfo {
        name: "label".to_owned(),
        description: String::new(),
        port_kind: crate::core::graph::PortKind::Data,
        delivery_profile: Some("newest".to_owned()),
        borrow_quota: None,
        audio_window: None,
        pin_default: None,
        pin: crate::core::descriptors::PinDeclaration::exactly(pin_type).into_resolved(),
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(PortFamily::Channels),
    }
}

#[test]
fn a_typed_value_a_process_nodes_read_cannot_carry_is_refused_and_a_function_or_shader_node_takes_it()
 {
    use crate::core::descriptors::{FunctionNodeLanguage, PinType};
    let text_input = a_channels_input_typed(PinType::String {});
    let refusal = refuse_a_value_the_input_port_does_not_take(
        "mixer",
        Some(NodeKind::Process),
        &text_input,
        &json!("loud"),
    )
    .expect_err("a process node's read carries no text");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
    assert!(refusal.words.contains("`label`"), "{refusal}");
    for reads_typed_values in [
        NodeKind::Shader,
        NodeKind::Function {
            language: FunctionNodeLanguage::Python,
        },
    ] {
        assert_eq!(
            refuse_a_value_the_input_port_does_not_take(
                "mixer",
                Some(reads_typed_values),
                &text_input,
                &json!("loud"),
            ),
            Ok(()),
            "{reads_typed_values:?}"
        );
    }
    assert_eq!(
        refuse_a_value_the_input_port_does_not_take(
            "mixer",
            Some(NodeKind::Process),
            &a_channels_input_typed(PinType::Float { exact_size: None }),
            &json!(0.5),
        ),
        Ok(()),
        "a number reaches a process node"
    );
}

/// A setting taken off a node is a config field taken off, which the config path makes, so a
/// setting a node also exposes as an input is taken off as any other is.
#[test]
fn a_setting_is_taken_off_as_a_config_field() {
    assert_eq!(
        the_refusal_of(
            &MockSourceTakingOneSetting::node_class_import_path(),
            "frame_width",
            None
        ),
        Ok(())
    );
}
