// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::NodeClassImportPath;

mod bundle_tests;
mod connection_tests;

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
        flow: None,
    }
}

/// The type of the node every input end of these tests is on.
fn the_sinks_node_type() -> &'static NodeClassImportPath {
    static SINK: std::sync::LazyLock<NodeClassImportPath> =
        std::sync::LazyLock::new(|| NodeClassImportPath::new("nodes.sink:Sink").unwrap());
    &SINK
}

fn an_input(pin_type: &PinType) -> PinLinkInputEnd<'_> {
    PinLinkInputEnd {
        end: PinLinkEnd {
            node_name: "sink",
            port_name: "in",
            ..an_output(pin_type)
        },
        node_type: the_sinks_node_type(),
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
        output_picked_for_the_input: None,
        saved_value_cast: None,
    }
}

fn the_conversions_named_by(refusal: &PinLinkRefusal) -> Vec<NodeClassImportPath> {
    match refusal {
        PinLinkRefusal::PinTypesDoNotConnect {
            conversions_that_bridge,
            ..
        } => conversions_that_bridge
            .iter()
            .map(|conversion| conversion.node_type.clone())
            .collect(),
        other => panic!("a refusal by types, not {other:?}"),
    }
}

#[test]
fn the_same_type_connects_as_it_is_whatever_it_is() {
    let bundle = PinType::Bundle {
        name: None,
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

/// A Texture output stating a format the input does not take goes through a conversion of
/// Textures that bridges the two, and without one is refused in words naming each side's formats.
#[test]
fn a_texture_of_a_format_the_input_does_not_take_converts_or_is_refused_naming_the_formats() {
    use TexturePixelFormat::{Bgra8Unorm, Nv12, Rgba8Unorm};
    let output = texture(&[Bgra8Unorm, Rgba8Unorm]);
    let input = texture(&[Rgba8Unorm]);
    let format_convert = a_conversion(
        NodeClassImportPath::of_built_in_node("TextureFormatConvert"),
        texture(&[Rgba8Unorm, Bgra8Unorm]),
        texture(&[Rgba8Unorm]),
    );

    let verdict = pin_link_verdict(
        an_output(&output),
        an_input(&input),
        std::slice::from_ref(&format_convert),
    );
    let PinLinkOutcome::ConnectsThroughConvertNode(convert_node) = &verdict.outcome else {
        panic!("a conversion of Textures bridges them: {verdict:?}");
    };
    assert_eq!(convert_node.conversion, format_convert);
    assert!(verdict.warnings.is_empty(), "{verdict:?}");

    let nv12 = texture(&[Nv12]);
    let PinLinkOutcome::Refused(refusal) = pin_link_verdict(
        an_output(&nv12),
        an_input(&input),
        std::slice::from_ref(&format_convert),
    )
    .outcome
    else {
        panic!("the conversion takes no nv12 Texture");
    };
    assert!(the_conversions_named_by(&refusal).is_empty(), "{refusal}");
    assert_eq!(
        refusal.to_string(),
        "cannot link `source.out` (Texture (nv12)) to `sink.in` (Texture (rgba8_unorm)): a \
         Texture (nv12) output does not connect to a Texture (rgba8_unorm) input; no node type \
         converts between them"
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

fn a_setting(read: SettingReadTime, pin_type: &PinType) -> PinLinkInputEnd<'_> {
    PinLinkInputEnd {
        input_mode: Some(NodeInputMode::SettingOnly { read }),
        ..an_input(pin_type)
    }
}

fn a_value_output(pin_type: &PinType) -> PinLinkEnd<'_> {
    PinLinkEnd {
        flow: Some(PinFlow::Value),
        ..an_output(pin_type)
    }
}

#[test]
fn a_value_wire_into_a_setting_read_at_setup_connects_as_its_types_do() {
    for output in [an_output(&float()), a_value_output(&float())] {
        let verdict = pin_link_verdict(output, a_setting(SettingReadTime::AtSetup, &float()), &[]);
        assert_eq!(verdict.outcome, PinLinkOutcome::Connects, "{verdict:?}");
    }
    let verdict = pin_link_verdict(
        a_value_output(&float()),
        a_setting(SettingReadTime::AtSetup, &integer()),
        &[],
    );
    assert_eq!(
        verdict.outcome,
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers)
    );
}

#[test]
fn a_stream_or_a_trigger_into_a_setting_is_refused_naming_the_sampling_fix() {
    for (output_flow, pin_type, read) in [
        (PinFlow::Stream, float(), SettingReadTime::AtSetup),
        (
            PinFlow::Trigger,
            PinType::Trigger {},
            SettingReadTime::AtSetup,
        ),
        (PinFlow::Stream, float(), SettingReadTime::EachRun),
        (
            PinFlow::Trigger,
            PinType::Trigger {},
            SettingReadTime::EachRun,
        ),
    ] {
        let verdict = pin_link_verdict(
            PinLinkEnd {
                flow: Some(output_flow),
                ..an_output(&pin_type)
            },
            a_setting(read, &float()),
            &[],
        );
        let PinLinkOutcome::Refused(refusal) = verdict.outcome else {
            panic!("a setting takes a Value wire only: {verdict:?}");
        };
        let PinLinkRefusal::StreamIntoASetting {
            output_flow: refused_flow,
            node_name,
            input_name,
            ..
        } = refusal.as_ref()
        else {
            panic!("refused as a stream into a setting: {refusal:?}");
        };
        assert_eq!(
            (*refused_flow, node_name.as_str(), input_name.as_str()),
            (output_flow, "sink", "in")
        );
        let words = refusal.to_string();
        assert!(
            words.contains("setting `in` of `sink`")
                && words.contains(&format!("flows as `{}`", output_flow.name()))
                && words.contains("place a node that samples"),
            "{words}"
        );
    }
}

#[test]
fn a_stream_into_an_input_read_each_run_connects_as_its_types_do() {
    let verdict = pin_link_verdict(
        PinLinkEnd {
            flow: Some(PinFlow::Stream),
            ..an_output(&float())
        },
        an_input(&float()),
        &[],
    );
    assert_eq!(verdict.outcome, PinLinkOutcome::Connects);
}

/// Mental-revert: refusing every wire into a setting read each run refuses a Value wire the
/// engine reads into it as it does one into a setting read at setup.
#[test]
fn a_value_wire_into_a_setting_read_each_run_connects_as_its_types_do() {
    let verdict = pin_link_verdict(
        a_value_output(&float()),
        a_setting(SettingReadTime::EachRun, &float()),
        &[],
    );
    assert_eq!(verdict.outcome, PinLinkOutcome::Connects);
}

#[test]
fn a_setting_of_a_type_no_message_carries_takes_no_wire() {
    let node_reference = PinType::NodeReference {
        referenced_node_type: None,
        one_referrer_per_referenced_node: false,
    };
    let verdict = pin_link_verdict(
        a_value_output(&node_reference),
        a_setting(SettingReadTime::AtSetup, &node_reference),
        &[],
    );
    let PinLinkOutcome::Refused(refusal) = verdict.outcome else {
        panic!("a node reference takes no wire: {verdict:?}");
    };
    assert_eq!(*refusal, PinLinkRefusal::input_takes_no_wire("sink", "in"));
}

/// A setting read at setup of each node type of the graph itself — `GraphInput`,
/// `GraphOutput`, `SetVariable` and `GetVariable` — takes no wire, though the same setting on
/// another node takes a Value wire; an input of one of them that is no setting still takes one.
///
/// Mental-revert: exempting a `GraphInput`'s settings alone connects the `GraphOutput`'s, the
/// `SetVariable`'s and the `GetVariable`'s.
#[test]
fn a_setting_of_a_node_type_of_the_graph_itself_takes_no_wire() {
    for node_type in [
        crate::GRAPH_INPUT_NODE_TYPE,
        crate::GRAPH_OUTPUT_NODE_TYPE,
        crate::SET_VARIABLE_NODE_TYPE,
        crate::GET_VARIABLE_NODE_TYPE,
    ] {
        let node_type = NodeClassImportPath::new(node_type).unwrap();
        let verdict = pin_link_verdict(
            a_value_output(&float()),
            PinLinkInputEnd {
                node_type: &node_type,
                ..a_setting(SettingReadTime::AtSetup, &float())
            },
            &[],
        );
        let PinLinkOutcome::Refused(refusal) = verdict.outcome else {
            panic!("a setting of {node_type} takes no wire: {verdict:?}");
        };
        assert_eq!(*refusal, PinLinkRefusal::input_takes_no_wire("sink", "in"));

        assert!(
            an_input_takes_a_wire(&node_type, Some(NodeInputMode::WireOrValue), Some(&float())),
            "an input of {node_type} that is no setting takes a wire"
        );
        assert!(
            an_input_takes_a_wire(&node_type, None, None),
            "a port of {node_type} takes a wire"
        );
    }
    assert!(an_input_takes_a_wire(
        the_sinks_node_type(),
        Some(NodeInputMode::SettingOnly {
            read: SettingReadTime::AtSetup
        }),
        Some(&float())
    ));
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

/// A refusal between two unencoded types no stated conversion bridges names no node.
///
/// Mental-revert: naming a node by the two ends' families tells a person to place a
/// `ChannelsToRecords`, which the runtime does not register.
#[test]
fn a_refusal_no_stated_conversion_bridges_names_no_node() {
    let PinLinkOutcome::Refused(refusal) =
        the_outcome_between(&PinType::Audio {}, &PinType::String {})
    else {
        panic!("Audio does not connect to a String");
    };
    let words = refusal.to_string();
    assert!(
        words.contains("no node type converts between them"),
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

/// A conversion whose input states the formats it takes bridges only a Texture of those; an
/// `nv12` Texture into a Tensor is refused naming no node.
#[test]
fn a_texture_of_a_format_no_conversion_takes_is_refused_naming_no_node() {
    use TexturePixelFormat::{Nv12, Rgba8Unorm};
    let texture_to_tensor = a_conversion(
        NodeClassImportPath::of_built_in_node("TextureToTensor"),
        texture(&[Rgba8Unorm]),
        PinType::Tensor {},
    );
    let verdict_from = |output_format| {
        pin_link_verdict(
            an_output(&texture(&[output_format])),
            an_input(&PinType::Tensor {}),
            std::slice::from_ref(&texture_to_tensor),
        )
    };

    assert!(
        matches!(
            verdict_from(Rgba8Unorm).outcome,
            PinLinkOutcome::ConnectsThroughConvertNode(_)
        ),
        "an rgba8_unorm Texture goes through the conversion"
    );
    let PinLinkOutcome::Refused(refusal) = verdict_from(Nv12).outcome else {
        panic!("no conversion takes an nv12 Texture");
    };
    assert!(the_conversions_named_by(&refusal).is_empty(), "{refusal}");
    let words = refusal.to_string();
    assert!(!words.contains("TextureToTensor"), "{words}");
    assert!(
        words.contains("no node type converts between them"),
        "{words}"
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

/// The words a refusal of a link from `output` into `input`, which no conversion bridges, says.
fn the_refusal_words_between(output: &PinType, input: &PinType) -> String {
    let PinLinkOutcome::Refused(refusal) = the_outcome_between(output, input) else {
        panic!("{output} does not connect to {input}");
    };
    refusal.to_string()
}

/// A refusal between an encoded stream and what it encodes names only an encoder or a decoder
/// of a codec the encoded end states, and none between two ends no codec node joins.
///
/// Mental-revert: naming the node by the two ends' families alone tells Opus packets into a
/// Texture to place a video decoder.
#[test]
fn a_refusal_names_an_encoder_or_decoder_only_of_a_codec_the_encoded_end_states() {
    let encoded_audio = |codecs: &[&str]| PinType::EncodedAudio {
        codecs: codecs.iter().map(|codec| codec.to_string()).collect(),
    };
    let any_texture = texture(&[]);
    for (output, input, placement) in [
        (
            encoded_audio(&["opus"]),
            any_texture.clone(),
            "; no node type converts between them",
        ),
        (
            encoded_video(&["h264"]),
            any_texture.clone(),
            "; place an H264Decoder node between them",
        ),
        (
            encoded_video(&[]),
            any_texture.clone(),
            "; place one of H264Decoder, H265Decoder between them",
        ),
        (
            encoded_video(&["vp9"]),
            any_texture.clone(),
            "; no node type converts between them",
        ),
        (
            encoded_video(&["h264", "h265"]),
            any_texture.clone(),
            "; no one decoder takes every codec the output states; narrow the output to one of \
             h264, h265, then place its decoder between them",
        ),
        (
            encoded_video(&["h264", "vp9"]),
            any_texture.clone(),
            "; no one decoder takes every codec the output states; narrow the output to h264, \
             then place an H264Decoder node between them",
        ),
        (
            encoded_video(&["vp9", "h265", "h264"]),
            any_texture.clone(),
            "; no one decoder takes every codec the output states; narrow the output to one of \
             h264, h265, then place its decoder between them",
        ),
        (
            any_texture.clone(),
            encoded_video(&["h264", "h265"]),
            "; place one of H264Encoder, H265Encoder between them",
        ),
        (
            any_texture.clone(),
            encoded_video(&["h265"]),
            "; place an H265Encoder node between them",
        ),
        (
            encoded_audio(&["opus"]),
            PinType::Audio {},
            "; place an OpusDecoder node between them",
        ),
        (
            PinType::Audio {},
            encoded_audio(&["aac"]),
            "; no node type converts between them",
        ),
        (
            float(),
            encoded_audio(&[]),
            "; no node type converts between them",
        ),
    ] {
        let words = the_refusal_words_between(&output, &input);
        assert!(words.ends_with(placement), "{output} into {input}: {words}");
    }
}

/// The `format` a Tensor-to-Texture conversion picks for the input it feeds: the first format
/// that input takes among the two it writes, `rgba8_unorm` for an input that takes any.
fn the_format_picked_for(input: &PinType) -> Option<PinConversionOutputPicked> {
    use TexturePixelFormat::{Bgra8Unorm, Rgba8Unorm};
    let PinType::Texture { pixel_formats } = input else {
        return None;
    };
    let format = match pixel_formats.as_slice() {
        [] => Rgba8Unorm,
        taken => *taken
            .iter()
            .find(|format| [Rgba8Unorm, Bgra8Unorm].contains(format))?,
    };
    Some(PinConversionOutputPicked {
        input_values: serde_json::Map::from_iter([(
            "format".to_owned(),
            serde_json::Value::from(format.wire_name()),
        )]),
        output_pin_type: texture(&[format]),
    })
}

fn a_tensor_to_texture_conversion_picking_its_format() -> PinConversion {
    PinConversion {
        output_picked_for_the_input: Some(PinConversionOutputPick {
            picked_for: the_format_picked_for,
            given_under: |_| None,
        }),
        ..a_conversion(
            NodeClassImportPath::of_built_in_node("TensorToTexture"),
            PinType::Tensor {},
            texture(&[]),
        )
    }
}

/// A conversion whose output follows a setting is judged by the type it gives under the setting
/// it picks for the input, and the convert node holds that setting.
///
/// Mental-revert: judging the output as it states itself, a Texture of no format, connects into
/// a `bgra8_unorm` input with a warning and holds no `format`.
#[test]
fn a_conversion_picks_the_setting_its_output_follows_for_the_input_it_feeds() {
    use TexturePixelFormat::{Bgra8Unorm, Nv12};
    let tensor_to_texture = a_tensor_to_texture_conversion_picking_its_format();

    let verdict = pin_link_verdict(
        an_output(&PinType::Tensor {}),
        an_input(&texture(&[Nv12, Bgra8Unorm])),
        std::slice::from_ref(&tensor_to_texture),
    );

    let PinLinkOutcome::ConnectsThroughConvertNode(convert_node) = &verdict.outcome else {
        panic!("the conversion writes bgra8_unorm: {verdict:?}");
    };
    assert_eq!(
        convert_node.input_values,
        serde_json::Map::from_iter([("format".to_owned(), "bgra8_unorm".into())])
    );
    assert!(verdict.warnings.is_empty(), "{verdict:?}");
}

/// A conversion whose settings give no type the input takes does not bridge the two ends, so the
/// link is refused naming no node.
///
/// Mental-revert: judging the output as it states itself offers the conversion into an `nv12`
/// input it can never write.
#[test]
fn a_conversion_whose_settings_give_no_type_the_input_takes_does_not_bridge() {
    let tensor_to_texture = a_tensor_to_texture_conversion_picking_its_format();

    let PinLinkOutcome::Refused(refusal) = pin_link_verdict(
        an_output(&PinType::Tensor {}),
        an_input(&texture(&[TexturePixelFormat::Nv12])),
        std::slice::from_ref(&tensor_to_texture),
    )
    .outcome
    else {
        panic!("the conversion writes no nv12 Texture");
    };
    assert!(the_conversions_named_by(&refusal).is_empty(), "{refusal}");
}
