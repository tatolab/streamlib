// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::NodeClassImportPath;

fn boolean() -> PinType {
    PinType::Boolean {}
}

fn integer() -> PinType {
    PinType::Integer { exact_size: None }
}

fn float() -> PinType {
    PinType::Float { exact_size: None }
}

fn vector(component_count: VectorComponentCount) -> PinType {
    PinType::Vector { component_count }
}

fn menu(option_names: &[&str]) -> PinType {
    PinType::Menu {
        option_names: option_names.iter().map(|name| name.to_string()).collect(),
    }
}

fn texture(pixel_formats: &[TexturePixelFormat]) -> PinType {
    PinType::Texture {
        pixel_formats: pixel_formats.to_vec(),
    }
}

fn encoded_video(codecs: &[&str]) -> PinType {
    PinType::EncodedVideo {
        codecs: codecs.iter().map(|codec| codec.to_string()).collect(),
    }
}

fn named(import_path: &str) -> PinType {
    PinType::Named {
        named_pin_type: NamedPinTypeImportPath::new(import_path).unwrap(),
    }
}

fn an_output(pin_type: &PinType) -> PinLinkEnd<'_> {
    PinLinkEnd {
        node_name: "source",
        port_name: "out",
        member_name: None,
        pin_type,
        meaning: None,
    }
}

fn an_input(pin_type: &PinType) -> PinLinkInputEnd<'_> {
    PinLinkInputEnd {
        end: PinLinkEnd {
            node_name: "sink",
            port_name: "in",
            ..an_output(pin_type)
        },
        input_mode: None,
    }
}

fn an_input_meaning<'end>(pin_type: &'end PinType, meaning: &'end str) -> PinLinkInputEnd<'end> {
    let mut input = an_input(pin_type);
    input.end.meaning = Some(meaning);
    input
}

/// The verdict of a link from an output of `output` to an input of `input`, with no
/// conversions.
fn the_verdict_between(output: &PinType, input: &PinType) -> PinLinkVerdict {
    pin_link_verdict(an_output(output), an_input(input), &[])
}

fn the_outcome_between(output: &PinType, input: &PinType) -> PinLinkOutcome {
    the_verdict_between(output, input).outcome
}

fn a_conversion(node_type: NodeClassImportPath, from: PinType, to: PinType) -> PinConversion {
    PinConversion {
        node_type,
        input_port_name: "in".to_owned(),
        input_pin_type: from,
        output_port_name: "out".to_owned(),
        output_pin_type: to,
    }
}

fn the_conversions_named_by(refusal: &PinLinkRefusal) -> Vec<NodeClassImportPath> {
    match refusal {
        PinLinkRefusal::PinTypesDoNotConnect {
            conversions_that_bridge,
            ..
        } => conversions_that_bridge.clone(),
        other => panic!("a refusal by types, not {other:?}"),
    }
}

#[test]
fn the_same_type_connects_as_it_is_whatever_it_is() {
    let bundle = PinType::Bundle {
        members: vec![BundleMember {
            name: "x".to_owned(),
            member_type: float(),
            meaning: None,
            default_value: None,
        }],
    };
    let list = PinType::ListOf {
        element_type: Box::new(float()),
    };
    for pin_type in [
        boolean(),
        float(),
        PinType::String {},
        PinType::Color {},
        vector(VectorComponentCount::Three),
        PinType::Rectangle {},
        PinType::Transform {},
        PinType::Audio {},
        PinType::Tensor {},
        PinType::Trigger {},
        list,
        bundle,
        named("module:Detections"),
        texture(&[]),
    ] {
        assert_eq!(
            the_verdict_between(&pin_type, &pin_type),
            PinLinkVerdict {
                outcome: PinLinkOutcome::Connects,
                warnings: Vec::new(),
            },
            "{pin_type}"
        );
    }
}

#[test]
fn numbers_convert_among_themselves_for_free() {
    let sized_integer = PinType::Integer {
        exact_size: Some(IntegerSize::Int16),
    };
    for (output, input) in [
        (boolean(), integer()),
        (boolean(), float()),
        (integer(), boolean()),
        (integer(), float()),
        (float(), integer()),
        (float(), boolean()),
        (integer(), sized_integer),
    ] {
        assert_eq!(
            the_outcome_between(&output, &input),
            PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers),
            "{output} to {input}"
        );
    }
}

#[test]
fn vectors_resize_read_as_a_color_and_take_one_number_spread_over_them_for_free() {
    use VectorComponentCount::{Four, Two};
    assert_eq!(
        the_outcome_between(&vector(Two), &vector(Four)),
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenVectors)
    );
    assert_eq!(
        the_verdict_between(&vector(Four), &PinType::Color {}),
        PinLinkVerdict {
            outcome: PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::VectorIntoColor),
            warnings: vec![PinLinkWarning::VectorReadAsAColor],
        }
    );
    assert_eq!(
        the_outcome_between(&float(), &vector(Two)),
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::NumberSpreadOverVector)
    );
    assert_eq!(
        the_outcome_between(&boolean(), &PinType::Color {}),
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::NumberSpreadOverColor)
    );
}

#[test]
fn a_menu_reads_as_a_string_and_warns_of_options_the_input_does_not_list() {
    assert_eq!(
        the_outcome_between(&menu(&["sine"]), &PinType::String {}),
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::MenuOptionNameAsString)
    );
    assert_eq!(
        the_verdict_between(&menu(&["sine", "saw"]), &menu(&["sine", "square"])),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![PinLinkWarning::MenuOptionsTheInputDoesNotList {
                option_names: vec!["saw".to_owned()],
            }],
        }
    );
}

/// A conversion holds one way only: a String is not a Menu, a Color is not a Vector, a Vector
/// is not a number.
#[test]
fn a_free_conversion_runs_one_way_only() {
    for (output, input) in [
        (PinType::String {}, menu(&["sine"])),
        (PinType::Color {}, vector(VectorComponentCount::Four)),
        (vector(VectorComponentCount::Two), float()),
        (float(), PinType::String {}),
    ] {
        assert!(
            matches!(
                the_outcome_between(&output, &input),
                PinLinkOutcome::Refused(_)
            ),
            "{output} to {input}"
        );
    }
}

#[test]
fn a_texture_connects_when_the_input_takes_every_format_the_output_states() {
    use TexturePixelFormat::{Bgra8Unorm, Nv12, Rgba8Unorm};
    assert_eq!(
        the_outcome_between(&texture(&[Rgba8Unorm]), &texture(&[Rgba8Unorm, Bgra8Unorm])),
        PinLinkOutcome::Connects
    );
    assert_eq!(
        the_outcome_between(&texture(&[Rgba8Unorm, Nv12]), &texture(&[])),
        PinLinkOutcome::Connects
    );
    assert!(
        matches!(
            the_outcome_between(&texture(&[Rgba8Unorm, Nv12]), &texture(&[Rgba8Unorm])),
            PinLinkOutcome::Refused(_)
        ),
        "an overlap is not enough: the output may still send nv12"
    );
    assert_eq!(
        the_verdict_between(&texture(&[]), &texture(&[Rgba8Unorm])),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![PinLinkWarning::TextureOutputStatesNoFormat],
        }
    );
}

#[test]
fn an_encoded_stream_connects_when_the_input_takes_every_codec_the_output_states() {
    assert_eq!(
        the_outcome_between(&encoded_video(&["h264"]), &encoded_video(&[])),
        PinLinkOutcome::Connects
    );
    assert_eq!(
        the_outcome_between(&encoded_video(&["h264"]), &encoded_video(&["h264", "h265"])),
        PinLinkOutcome::Connects
    );
    for output in [encoded_video(&[]), encoded_video(&["h265"])] {
        assert!(
            matches!(
                the_outcome_between(&output, &encoded_video(&["h264"])),
                PinLinkOutcome::Refused(_)
            ),
            "{output}"
        );
    }
}

#[test]
fn two_named_types_connect_only_under_one_path() {
    assert!(matches!(
        the_outcome_between(&named("module:Detections"), &named("module:Poses")),
        PinLinkOutcome::Refused(_)
    ));
}

#[test]
fn different_meanings_warn_and_never_block() {
    let verdict = pin_link_verdict(
        PinLinkEnd {
            meaning: Some("frequency"),
            ..an_output(&float())
        },
        an_input_meaning(&float(), "gain"),
        &[],
    );
    assert_eq!(
        verdict,
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![PinLinkWarning::MeaningsDiffer {
                output_meaning: "frequency".to_owned(),
                input_meaning: "gain".to_owned(),
            }],
        }
    );
}

#[test]
fn a_link_into_a_setting_is_refused_whatever_the_types() {
    let verdict = pin_link_verdict(
        an_output(&float()),
        PinLinkInputEnd {
            input_mode: Some(NodeInputMode::SettingOnly {
                read: SettingReadTime::AtSetup,
            }),
            ..an_input(&float())
        },
        &[],
    );
    let PinLinkOutcome::Refused(refusal) = verdict.outcome else {
        panic!("a setting takes no wire: {verdict:?}");
    };
    assert_eq!(refusal, PinLinkRefusal::input_takes_no_wire("sink", "in"));
    assert!(
        refusal
            .to_string()
            .contains("input `in` of `sink`, a setting"),
        "{refusal}"
    );
}

#[test]
fn a_setting_only_type_connects_to_nothing() {
    let node_reference = PinType::NodeReference {
        referenced_node_type: None,
        one_referrer_per_referenced_node: false,
    };
    assert!(matches!(
        the_outcome_between(&node_reference, &node_reference),
        PinLinkOutcome::Refused(_)
    ));
}

#[test]
fn a_refusal_names_both_ends_and_both_types() {
    let PinLinkOutcome::Refused(refusal) = pin_link_verdict(
        PinLinkEnd {
            member_name: Some("label"),
            ..an_output(&PinType::String {})
        },
        an_input(&PinType::Tensor {}),
        &[],
    )
    .outcome
    else {
        panic!("a String does not connect to a Tensor");
    };
    let words = refusal.to_string();
    assert!(words.contains("`source.out.label` (String)"), "{words}");
    assert!(words.contains("`sink.in` (Tensor)"), "{words}");
    assert!(
        words.contains("no node type converts between them"),
        "{words}"
    );
}

/// Until the built-ins state their conversions, a refusal between two families one node
/// converts between names the node the family check names.
#[test]
fn a_refusal_no_stated_conversion_bridges_names_the_node_between_the_families() {
    let PinLinkOutcome::Refused(refusal) =
        the_outcome_between(&PinType::Audio {}, &PinType::String {})
    else {
        panic!("Audio does not connect to a String");
    };
    let words = refusal.to_string();
    assert!(
        words.contains("place a ChannelsToRecords node between them"),
        "{words}"
    );
}

/// A link through a convert node warns of what either side of the node warns of, and of
/// meanings that differ.
#[test]
fn a_link_through_a_convert_node_warns_of_what_each_side_warns_of() {
    let color_to_texture = a_conversion(
        NodeClassImportPath::of_built_in_node("ColorToTexture"),
        PinType::Color {},
        texture(&[]),
    );
    let verdict = pin_link_verdict(
        PinLinkEnd {
            meaning: Some("tint"),
            ..an_output(&vector(VectorComponentCount::Four))
        },
        an_input_meaning(&texture(&[TexturePixelFormat::Rgba8Unorm]), "mask"),
        std::slice::from_ref(&color_to_texture),
    );
    assert!(
        matches!(
            verdict.outcome,
            PinLinkOutcome::ConnectsThroughConvertNode(_)
        ),
        "{verdict:?}"
    );
    assert_eq!(
        verdict.warnings,
        vec![
            PinLinkWarning::VectorReadAsAColor,
            PinLinkWarning::TextureOutputStatesNoFormat,
            PinLinkWarning::MeaningsDiffer {
                output_meaning: "tint".to_owned(),
                input_meaning: "mask".to_owned(),
            },
        ]
    );
}

/// One built-in conversion that bridges the two ends is placed between them; the refusal of
/// the link without it names it.
#[test]
fn a_link_one_built_in_conversion_bridges_goes_through_its_node() {
    let texture_to_tensor = a_conversion(
        NodeClassImportPath::of_built_in_node("TextureToTensor"),
        texture(&[]),
        PinType::Tensor {},
    );
    let unrelated = a_conversion(
        NodeClassImportPath::of_built_in_node("AudioToTensor"),
        PinType::Audio {},
        PinType::Tensor {},
    );
    let verdict = pin_link_verdict(
        an_output(&texture(&[TexturePixelFormat::Rgba8Unorm])),
        an_input(&PinType::Tensor {}),
        &[unrelated, texture_to_tensor.clone()],
    );
    let PinLinkOutcome::ConnectsThroughConvertNode(convert_node) = &verdict.outcome else {
        panic!("a built-in conversion bridges them: {verdict:?}");
    };
    assert_eq!(convert_node.conversion, texture_to_tensor);
    let refusal = verdict
        .refusal_of_the_link_made_as_it_is()
        .expect("the link alone does not connect");
    assert_eq!(
        the_conversions_named_by(refusal),
        vec![NodeClassImportPath::of_built_in_node("TextureToTensor")]
    );
    assert!(
        refusal
            .to_string()
            .contains("place a `tatolab.stream:TextureToTensor` node between them"),
        "{refusal}"
    );
}

/// A conversion's sides are each judged by the direct rules, free conversions included: a
/// Boolean reaches a conversion that takes a Float.
#[test]
fn each_side_of_a_conversion_is_judged_by_the_direct_rules() {
    let float_to_texture = a_conversion(
        NodeClassImportPath::of_built_in_node("FloatToTexture"),
        float(),
        texture(&[TexturePixelFormat::Rgba8Unorm]),
    );
    let outcome = pin_link_verdict(
        an_output(&boolean()),
        an_input(&texture(&[])),
        std::slice::from_ref(&float_to_texture),
    )
    .outcome;
    assert!(
        matches!(outcome, PinLinkOutcome::ConnectsThroughConvertNode(_)),
        "{outcome:?}"
    );
}

/// A conversion a module declares is never placed: the link is refused naming it, as it is when
/// two built-in conversions would both bridge the ends.
#[test]
fn a_link_no_one_built_in_conversion_bridges_is_refused_naming_each_that_does() {
    let module_encoder = a_conversion(
        NodeClassImportPath::new("tatolab_codecs:VideoEncoder").unwrap(),
        texture(&[]),
        encoded_video(&["h264"]),
    );
    let PinLinkOutcome::Refused(refusal) = pin_link_verdict(
        an_output(&texture(&[])),
        an_input(&encoded_video(&[])),
        std::slice::from_ref(&module_encoder),
    )
    .outcome
    else {
        panic!("a module's conversion is never placed");
    };
    assert_eq!(
        the_conversions_named_by(&refusal),
        vec![module_encoder.node_type.clone()]
    );

    let both = [
        a_conversion(
            NodeClassImportPath::of_built_in_node("FirstEncoder"),
            texture(&[]),
            encoded_video(&[]),
        ),
        a_conversion(
            NodeClassImportPath::of_built_in_node("SecondEncoder"),
            texture(&[]),
            encoded_video(&[]),
        ),
    ];
    let PinLinkOutcome::Refused(refusal) = pin_link_verdict(
        an_output(&texture(&[])),
        an_input(&encoded_video(&[])),
        &both,
    )
    .outcome
    else {
        panic!("two built-in conversions leave no one to place");
    };
    assert_eq!(the_conversions_named_by(&refusal).len(), 2);
    assert!(refusal.to_string().contains("place one of"), "{refusal}");
}

#[test]
fn a_link_that_connects_directly_needs_no_conversion_even_when_one_bridges() {
    let float_to_float = a_conversion(
        NodeClassImportPath::of_built_in_node("Smooth"),
        float(),
        float(),
    );
    assert_eq!(
        pin_link_verdict(
            an_output(&float()),
            an_input(&float()),
            std::slice::from_ref(&float_to_float)
        )
        .outcome,
        PinLinkOutcome::Connects
    );
}

#[test]
fn a_type_reads_with_what_narrows_it() {
    assert_eq!(
        vector(VectorComponentCount::Three).to_string(),
        "Vector of 3"
    );
    assert_eq!(
        texture(&[TexturePixelFormat::Rgba8Unorm]).to_string(),
        "Texture (rgba8_unorm)"
    );
    assert_eq!(
        PinType::ListOf {
            element_type: Box::new(float())
        }
        .to_string(),
        "List of Float"
    );
    assert_eq!(
        PinType::Integer {
            exact_size: Some(IntegerSize::UInt8)
        }
        .to_string(),
        "Integer (UInt8)"
    );
    assert_eq!(
        PinType::Asset {
            media: vec![ProjectAssetMedia::Image]
        }
        .to_string(),
        "Asset (image)"
    );
}

/// Bundle members are compared by name and type; a member meaning that differs only warns.
#[test]
fn bundles_connect_by_member_names_and_types_and_warn_of_member_meanings() {
    let bundle = |meaning: &str| PinType::Bundle {
        members: vec![BundleMember {
            name: "pitch".to_owned(),
            member_type: float(),
            meaning: Some(meaning.to_owned()),
            default_value: None,
        }],
    };
    assert_eq!(
        the_verdict_between(&bundle("frequency"), &bundle("note")),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![PinLinkWarning::MemberMeaningsDiffer {
                member_name: "pitch".to_owned(),
                output_meaning: "frequency".to_owned(),
                input_meaning: "note".to_owned(),
            }],
        }
    );
    let list_of = |element_type| PinType::ListOf {
        element_type: Box::new(element_type),
    };
    assert_eq!(
        the_outcome_between(&list_of(bundle("frequency")), &list_of(bundle("note"))),
        PinLinkOutcome::Connects
    );
    let renamed = PinType::Bundle {
        members: vec![BundleMember {
            name: "gain".to_owned(),
            member_type: float(),
            meaning: None,
            default_value: None,
        }],
    };
    assert!(matches!(
        the_outcome_between(&bundle("frequency"), &renamed),
        PinLinkOutcome::Refused(_)
    ));
    assert_eq!(bundle("frequency").to_string(), "Bundle (pitch: Float)");
}
