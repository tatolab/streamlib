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
        None,
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
        refuse_a_value_holding_a_number_outside_the_range("mixer", "gain", &json!(0.5), range),
        Ok(())
    );
    let refusal =
        refuse_a_value_holding_a_number_outside_the_range("mixer", "gain", &json!(1.5), range)
            .expect_err("1.5 lies above the range");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
    assert!(refusal.words.contains("from 0 to 1"), "{refusal}");
}

/// A Channels input typed `pin_type`, named `label`.
fn a_channels_input_typed(pin_type: crate::core::descriptors::PinType) -> PortInfo {
    PortInfo {
        name: "label".to_owned(),
        description: String::new(),
        delivery_profile: Some("newest".to_owned()),
        borrow_quota: None,
        audio_window: None,
        pin_default: None,
        pin: crate::core::descriptors::PinDeclaration::exactly(pin_type).into_resolved(),
        declared_pin_type: None,
        lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
        former_names: Vec::new(),
        may_be_unset: false,
        input_mode: None,
        annotations: Default::default(),
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
    .expect_err("a process node's wired read takes no value");
    assert_eq!(refusal.code, InputValueRefusalCode::InputTakesNoValue);
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

/// A class node's field states that it takes a wire or a value, since its helper reads a value
/// set on it whole: a Color on its Channels input and an Asset on its Records input are taken on
/// a process node, held for its setup and its live sets, and a value its pin type refuses still
/// is.
///
/// Mental-revert: judging the field by a process node's Channels read alone refuses both, as
/// Solid Color's colour and Image File's asset were on the rig.
#[test]
fn a_process_nodes_input_stating_it_takes_a_wire_or_a_value_takes_any_value_of_its_pin_type() {
    use crate::core::descriptors::{NodeInputMode, PinType};
    let a_field = |pin_type: PinType| PortInfo {
        input_mode: Some(NodeInputMode::WireOrValue),
        ..a_channels_input_typed(pin_type)
    };
    let colour = a_field(PinType::Color {});
    let asset = a_field(PinType::Asset { media: Vec::new() });
    for (field, value) in [
        (
            &colour,
            json!({"red": 1.0, "green": 0.5, "blue": 0.0, "alpha": 1.0}),
        ),
        (
            &asset,
            json!({"asset": "0f0e0d0c-0b0a-4909-8807-060504030201"}),
        ),
    ] {
        assert!(the_input_port_takes_a_value(Some(NodeKind::Process), field));
        assert_eq!(
            refuse_a_value_the_input_port_does_not_take(
                "solidcolor",
                Some(NodeKind::Process),
                field,
                &value,
            ),
            Ok(()),
            "{value}"
        );
    }
    assert_eq!(
        refuse_a_value_the_input_port_does_not_take(
            "solidcolor",
            Some(NodeKind::Process),
            &colour,
            &json!("orange"),
        )
        .expect_err("a Color is no text")
        .code,
        InputValueRefusalCode::InputValueNotValid
    );
    assert!(
        !the_input_port_takes_a_value(
            Some(NodeKind::Process),
            &PortInfo {
                input_mode: None,
                ..asset.clone()
            }
        ),
        "an input stating no mode on a process node is read off its Channels alone"
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

/// In one live transaction a set of another member of a bundle input replaces nothing set on it,
/// and a set of the same member, of the whole input after a member, or of a member after the
/// whole input, replaces the earlier one, whichever order they come in.
///
/// Mental-revert: telling sets apart by their input alone refuses `gain` beside `level`.
#[test]
fn a_live_set_replaces_an_earlier_one_only_on_the_member_or_the_whole_input_it_sets() {
    assert!(!a_live_set_replaces_an_earlier_one(&[], None));
    assert!(!a_live_set_replaces_an_earlier_one(&[], Some("gain")));
    assert!(!a_live_set_replaces_an_earlier_one(
        &[Some("level")],
        Some("gain")
    ));
    assert!(a_live_set_replaces_an_earlier_one(
        &[Some("level"), Some("gain")],
        Some("gain")
    ));
    assert!(a_live_set_replaces_an_earlier_one(&[Some("level")], None));
    assert!(a_live_set_replaces_an_earlier_one(&[None], Some("level")));
    assert!(a_live_set_replaces_an_earlier_one(&[None], None));
}

/// A Records input taking a wire or a value, whose type its setting `kind` chooses, takes a
/// value of the bundle `kind` holds and refuses one that bundle does not take; while `kind`
/// holds nothing it takes no value, since no type says what one would be.
///
/// Mental-revert: judging the input by the pin it states, which is none, refuses every value
/// set on it, as MakeBundle's `members` was on the rig.
#[test]
fn an_input_whose_setting_chooses_a_bundle_takes_a_value_of_that_bundle() {
    use crate::core::descriptors::{BundleMember, DeclaredPinType, NodeInputMode, PinFlow, PinType};
    use crate::core::graph::PinTypeAsDeclared;
    let members = PortInfo {
        name: "members".to_owned(),
        pin: None,
        declared_pin_type: Some(PinTypeAsDeclared {
            pin_type: DeclaredPinType::SetByInput {
                input_name: "kind".to_owned(),
            },
            flow: PinFlow::Value,
        }),
        input_mode: Some(NodeInputMode::WireOrValue),
        ..a_channels_input_typed(PinType::String {})
    };
    let kind = PortInfo {
        name: "kind".to_owned(),
        ..a_channels_input_typed(PinType::a_choice_of_any())
    };
    let inputs = [members.clone(), kind];
    let reading = PinType::Bundle {
        name: None,
        members: vec![BundleMember {
            name: "level".to_owned(),
            member_type: PinType::Float { exact_size: None },
            meaning: None,
            default_value: None,
        }],
    };
    let holding = |kind: Option<&PinType>| {
        let mut values = Map::new();
        if let Some(kind) = kind {
            values.insert("kind".to_owned(), json!(kind));
        }
        values
    };
    let refusal_of = |kind: Option<&PinType>, value: Value| {
        refuse_a_value_the_input_port_does_not_take(
            "makebundle",
            Some(NodeKind::Process),
            &members.typed_by_its_setting_in(&inputs, &holding(kind)),
            &value,
        )
    };

    assert_eq!(refusal_of(Some(&reading), json!({"level": 0.5})), Ok(()));
    assert_eq!(
        refusal_of(Some(&reading), json!({"level": "loud"}))
            .expect_err("`level` is a Float")
            .code,
        InputValueRefusalCode::InputValueNotValid
    );
    assert_eq!(
        refusal_of(None, json!({"level": 0.5}))
            .expect_err("no type is chosen")
            .code,
        InputValueRefusalCode::InputTakesNoValue
    );
}

/// Make Bundle's `members`, its setting choosing a bundle that holds a Texture beside a Float
/// `level`, takes a value of `level` alone, and refuses one setting the picture, which only a
/// wire carries.
///
/// Mental-revert: judging a bundle holding a Texture wire-only as a whole refuses
/// `{"level": 0.5}` as taking no value.
#[test]
fn an_input_reading_a_bundle_holding_a_texture_takes_a_value_of_its_small_members() {
    use crate::core::descriptors::{BundleMember, DeclaredPinType, NodeInputMode, PinFlow, PinType, TexturePixelFormat};
    use crate::core::graph::PinTypeAsDeclared;
    let members = PortInfo {
        name: "members".to_owned(),
        pin: None,
        declared_pin_type: Some(PinTypeAsDeclared {
            pin_type: DeclaredPinType::SetByInput {
                input_name: "kind".to_owned(),
            },
            flow: PinFlow::Value,
        }),
        input_mode: Some(NodeInputMode::WireOrValue),
        ..a_channels_input_typed(PinType::String {})
    };
    let kind = PortInfo {
        name: "kind".to_owned(),
        ..a_channels_input_typed(PinType::a_choice_of_any())
    };
    let inputs = [members.clone(), kind];
    let a_member = |name: &str, member_type: PinType| BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    };
    let metered_picture = PinType::Bundle {
        name: None,
        members: vec![
            a_member(
                "picture",
                PinType::Texture {
                    pixel_formats: vec![TexturePixelFormat::Rgba8Unorm],
                },
            ),
            a_member("level", PinType::Float { exact_size: None }),
        ],
    };
    let mut holding = Map::new();
    holding.insert("kind".to_owned(), json!(metered_picture));
    let refusal_of = |value: Value| {
        refuse_a_value_the_input_port_does_not_take(
            "makebundle",
            Some(NodeKind::Process),
            &members.typed_by_its_setting_in(&inputs, &holding),
            &value,
        )
    };

    assert_eq!(refusal_of(json!({"level": 0.5})), Ok(()));
    assert_eq!(
        refusal_of(json!({"level": 0.5, "picture": {"surface_id": 1}}))
            .expect_err("the picture takes only a wire")
            .code,
        InputValueRefusalCode::InputValueNotValid
    );
}
