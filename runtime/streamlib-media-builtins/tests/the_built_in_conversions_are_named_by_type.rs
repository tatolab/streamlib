// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node catalog of a runtime built with the media built-ins lists each conversion that has a
//! typed form under the pin types it converts between, with typed pins and its conversion
//! stated, so the link verdict finds it between two ends.

use streamlib::sdk::descriptors::{
    BuiltInPinTypeName, BundleMember, NamedPinTypeImportPath, NodeClassImportPath, NodeDescriptor,
    PinConversion, PinLinkEnd, PinLinkInputEnd, PinLinkOutcome, PinLinkVerdict, PinLinkWarning,
    PinType, ProjectAssetMedia, TexturePixelFormat, VectorComponentCount, pin_link_verdict,
};
use streamlib::sdk::nodes::NODE_REGISTRY;
use streamlib_media_builtins::register_media_builtin_node_types;

/// The formats one `[h, w, 4]` tensor holds a pixel of, which `TextureToTensor` takes.
const ONE_TENSOR_PIXEL_FORMATS: [TexturePixelFormat; 6] = [
    TexturePixelFormat::Rgba8Unorm,
    TexturePixelFormat::Rgba8UnormSrgb,
    TexturePixelFormat::Bgra8Unorm,
    TexturePixelFormat::Bgra8UnormSrgb,
    TexturePixelFormat::Rgba16Float,
    TexturePixelFormat::Rgba32Float,
];

fn a_texture_of(pixel_formats: &[TexturePixelFormat]) -> PinType {
    PinType::Texture {
        pixel_formats: pixel_formats.to_vec(),
    }
}

/// Each built-in conversion with a typed form, as it states itself.
fn the_typed_conversions() -> [PinConversion; 5] {
    let conversion = |name: &str,
                      input_port_name: &str,
                      input_pin_type,
                      output_port_name: &str,
                      output_pin_type| {
        PinConversion {
            node_type: NodeClassImportPath::of_built_in_node(name),
            input_port_name: input_port_name.to_owned(),
            input_pin_type,
            output_port_name: output_port_name.to_owned(),
            output_pin_type,
            output_picked_for_the_input: None,
            saved_value_cast: None,
        }
    };
    [
        conversion(
            "TextureToTensor",
            "image",
            a_texture_of(&ONE_TENSOR_PIXEL_FORMATS),
            "array",
            PinType::Tensor {},
        ),
        conversion(
            "TensorToTexture",
            "array",
            PinType::Tensor {},
            "image",
            a_texture_of(&[]),
        ),
        conversion(
            "TextureFormatConvert",
            "image",
            a_texture_of(&[
                TexturePixelFormat::Rgba8Unorm,
                TexturePixelFormat::Bgra8Unorm,
            ]),
            "rgba_image",
            a_texture_of(&[TexturePixelFormat::Rgba8Unorm]),
        ),
        conversion(
            "AudioToTensor",
            "channels",
            PinType::Audio {},
            "array",
            PinType::Tensor {},
        ),
        conversion(
            "TensorToAudio",
            "array",
            PinType::Tensor {},
            "channels",
            PinType::Audio {},
        ),
    ]
}

fn the_registered_built_ins() -> Vec<NodeDescriptor> {
    register_media_builtin_node_types();
    NODE_REGISTRY
        .list_registered()
        .into_iter()
        .filter(|descriptor| descriptor.node_class_import_path.names_a_built_in_node())
        .collect()
}

fn the_conversions_stated_by(built_ins: &[NodeDescriptor]) -> Vec<PinConversion> {
    built_ins
        .iter()
        .filter_map(|descriptor| {
            descriptor
                .pin_conversion()
                .unwrap_or_else(|refusal| panic!("{refusal}"))
        })
        .collect()
}

#[test]
fn each_typed_conversion_is_listed_under_its_type_name_with_typed_pins_and_its_conversion() {
    let built_ins = the_registered_built_ins();
    let conversions = the_conversions_stated_by(&built_ins);

    for expected in the_typed_conversions() {
        let node_type = &expected.node_type;
        let descriptor = built_ins
            .iter()
            .find(|descriptor| &descriptor.node_class_import_path == node_type)
            .unwrap_or_else(|| panic!("the catalog lists no {node_type}"));
        assert!(descriptor.conversion, "{node_type} states no conversion");
        assert_eq!(
            conversions
                .iter()
                .find(|conversion| &conversion.node_type == node_type),
            Some(&expected),
            "{node_type}"
        );
    }
}

#[test]
fn no_built_in_is_listed_under_a_retired_conversion_name() {
    let built_ins = the_registered_built_ins();

    for retired_name in [
        "ImageToArray",
        "ArrayToImage",
        "ChannelsToArray",
        "ArrayToChannels",
    ] {
        let retired_type = NodeClassImportPath::of_built_in_node(retired_name);
        assert!(
            built_ins
                .iter()
                .all(|descriptor| descriptor.node_class_import_path != retired_type),
            "the catalog still lists {retired_type}"
        );
    }
}

#[test]
fn the_verdict_finds_each_typed_conversion_between_the_two_types_it_converts() {
    let conversions = the_conversions_stated_by(&the_registered_built_ins());

    for expected in the_typed_conversions() {
        let verdict = the_verdict_between(
            &expected.input_pin_type,
            &expected.output_pin_type,
            &conversions,
        );

        match verdict.outcome {
            PinLinkOutcome::ConnectsThroughConvertNode(convert_node) => {
                assert_eq!(convert_node.conversion.node_type, expected.node_type)
            }
            other => panic!(
                "{} into {}: {other:?}",
                expected.input_pin_type, expected.output_pin_type
            ),
        }
    }
}

/// An `nv12` Texture has no one-tensor layout, so no conversion is offered into a Tensor.
#[test]
fn the_verdict_offers_no_conversion_from_an_nv12_texture_into_a_tensor() {
    let conversions = the_conversions_stated_by(&the_registered_built_ins());
    let nv12 = a_texture_of(&[TexturePixelFormat::Nv12]);

    let verdict = the_verdict_between(&nv12, &PinType::Tensor {}, &conversions);

    assert!(
        matches!(verdict.outcome, PinLinkOutcome::Refused(_)),
        "{verdict:?}"
    );
}

/// A `bgra8_unorm` Texture linked into an input that takes only `rgba8_unorm` gets a
/// `TextureFormatConvert`, which holds no setting.
///
/// Mental-revert: without the node registered, the link is refused.
#[test]
fn the_verdict_places_a_texture_format_convert_between_bgra8_and_rgba8() {
    let conversions = the_conversions_stated_by(&the_registered_built_ins());

    let verdict = the_verdict_between(
        &a_texture_of(&[TexturePixelFormat::Bgra8Unorm]),
        &a_texture_of(&[TexturePixelFormat::Rgba8Unorm]),
        &conversions,
    );

    match verdict.outcome {
        PinLinkOutcome::ConnectsThroughConvertNode(convert_node) => {
            assert_eq!(
                convert_node.conversion.node_type,
                NodeClassImportPath::of_built_in_node("TextureFormatConvert")
            );
            assert!(convert_node.input_values.is_empty(), "{convert_node:?}");
        }
        other => panic!("{other:?}"),
    }
}

/// A pair of formats `TextureFormatConvert` does not cover is refused, the refusal naming both
/// formats: one it takes no frame of, an sRGB one, and one into a format it does not write.
///
/// Mental-revert: declaring `rgba16_float` or `bgra8_unorm_srgb` on its input places the node.
#[test]
fn a_pair_of_formats_no_conversion_covers_is_refused_naming_both() {
    let conversions = the_conversions_stated_by(&the_registered_built_ins());

    for (output_format, input_format) in [
        (
            TexturePixelFormat::Rgba16Float,
            TexturePixelFormat::Rgba8Unorm,
        ),
        (
            TexturePixelFormat::Bgra8UnormSrgb,
            TexturePixelFormat::Rgba8Unorm,
        ),
        (
            TexturePixelFormat::Rgba8Unorm,
            TexturePixelFormat::Bgra8Unorm,
        ),
    ] {
        let verdict = the_verdict_between(
            &a_texture_of(&[output_format]),
            &a_texture_of(&[input_format]),
            &conversions,
        );

        let PinLinkOutcome::Refused(refusal) = verdict.outcome else {
            panic!("{output_format:?} into {input_format:?}: {verdict:?}");
        };
        let refusal = refusal.to_string();
        assert!(refusal.contains(output_format.wire_name()), "{refusal}");
        assert!(refusal.contains(input_format.wire_name()), "{refusal}");
    }
}

/// A Texture output that states no format connects to an input that takes only `rgba8_unorm`
/// with a warning, so no convert node is placed on a guess.
#[test]
fn a_texture_output_stating_no_format_connects_with_a_warning_and_no_convert_node() {
    let conversions = the_conversions_stated_by(&the_registered_built_ins());

    let verdict = the_verdict_between(
        &a_texture_of(&[]),
        &a_texture_of(&[TexturePixelFormat::Rgba8Unorm]),
        &conversions,
    );

    assert_eq!(verdict.outcome, PinLinkOutcome::Connects);
    assert_eq!(
        verdict.warnings,
        [PinLinkWarning::TextureOutputStatesNoFormat]
    );
}

fn the_verdict_between(
    output_pin_type: &PinType,
    input_pin_type: &PinType,
    conversions: &[PinConversion],
) -> PinLinkVerdict {
    let downstreams_type =
        streamlib::sdk::descriptors::NodeClassImportPath::new("nodes.downstream:Downstream")
            .unwrap();
    pin_link_verdict(
        PinLinkEnd {
            node_name: "upstream",
            port_name: "out",
            member_name: None,
            pin_type: output_pin_type,
            meaning: None,
            flow: None,
        },
        PinLinkInputEnd {
            end: PinLinkEnd {
                node_name: "downstream",
                port_name: "in",
                member_name: None,
                pin_type: input_pin_type,
                meaning: None,
                flow: None,
            },
            node_type: &downstreams_type,
            input_mode: None,
        },
        conversions,
    )
}

/// Every built-in pin type a conversion's ends are judged against: at least one of each type
/// [`BuiltInPinTypeName::EVERY_NAME`] lists, a type added there failing to compile here until it
/// is given one; each Texture format alone and a Texture of any; and each type a conversion states
/// at either end.
fn every_built_in_end_type(conversions: &[PinConversion]) -> Vec<PinType> {
    let float = || PinType::Float { exact_size: None };
    let mut end_types: Vec<PinType> = Vec::new();
    for name in BuiltInPinTypeName::EVERY_NAME {
        match name {
            BuiltInPinTypeName::Boolean => end_types.push(PinType::Boolean {}),
            BuiltInPinTypeName::Integer => end_types.push(PinType::Integer { exact_size: None }),
            BuiltInPinTypeName::Float => end_types.push(float()),
            BuiltInPinTypeName::String => end_types.push(PinType::String {}),
            BuiltInPinTypeName::Menu => end_types.push(PinType::Menu {
                option_names: vec!["low".to_owned(), "high".to_owned()],
            }),
            BuiltInPinTypeName::Color => end_types.push(PinType::Color {}),
            BuiltInPinTypeName::Vector => end_types.extend(
                [
                    VectorComponentCount::Two,
                    VectorComponentCount::Three,
                    VectorComponentCount::Four,
                ]
                .map(|component_count| PinType::Vector { component_count }),
            ),
            BuiltInPinTypeName::Rectangle => end_types.push(PinType::Rectangle {}),
            BuiltInPinTypeName::Transform => end_types.push(PinType::Transform {}),
            BuiltInPinTypeName::Texture => {
                end_types
                    .extend(TexturePixelFormat::EVERY_FORMAT.map(|format| a_texture_of(&[format])));
                end_types.push(a_texture_of(&[]));
            }
            BuiltInPinTypeName::Audio => end_types.push(PinType::Audio {}),
            BuiltInPinTypeName::EncodedVideo => {
                end_types.push(PinType::EncodedVideo { codecs: Vec::new() })
            }
            BuiltInPinTypeName::EncodedAudio => {
                end_types.push(PinType::EncodedAudio { codecs: Vec::new() })
            }
            BuiltInPinTypeName::Tensor => end_types.push(PinType::Tensor {}),
            BuiltInPinTypeName::Asset => end_types.extend([
                PinType::Asset { media: Vec::new() },
                PinType::Asset {
                    media: vec![ProjectAssetMedia::Image],
                },
            ]),
            BuiltInPinTypeName::Trigger => end_types.push(PinType::Trigger {}),
            BuiltInPinTypeName::ListOf => end_types.push(PinType::ListOf {
                element_type: Box::new(float()),
            }),
            BuiltInPinTypeName::Bundle => end_types.push(PinType::Bundle {
                members: vec![BundleMember {
                    name: "gain".to_owned(),
                    member_type: float(),
                    meaning: None,
                    default_value: None,
                }],
            }),
            BuiltInPinTypeName::Named => end_types.push(PinType::Named {
                named_pin_type: NamedPinTypeImportPath::new("conversion_check:Temperature")
                    .expect("a named pin type"),
            }),
            BuiltInPinTypeName::Connection => end_types.push(PinType::Connection {
                connection_type: NamedPinTypeImportPath::new("conversion_check:RelayConnection")
                    .expect("a connection type"),
                one_referrer_per_referenced_node: false,
            }),
            BuiltInPinTypeName::NodeReference => end_types.push(PinType::NodeReference {
                referenced_node_type: None,
                one_referrer_per_referenced_node: false,
            }),
            BuiltInPinTypeName::PinTypeChoice => {
                end_types.push(PinType::PinTypeChoice { among: Vec::new() })
            }
        }
    }
    for conversion in conversions {
        for stated in [&conversion.input_pin_type, &conversion.output_pin_type] {
            if !end_types.contains(stated) {
                end_types.push(stated.clone());
            }
        }
    }
    end_types
}

/// No two conversions `connect` may place bridge the same pair of built-in types, so a link never
/// has two built-in nodes to choose between. Each is judged by its declaration alone, which
/// bridges every pair the setting it picks for an input lets it bridge.
///
/// Mental-revert: a second built-in Tensor-to-Texture conversion makes two bridge a Tensor into a
/// Texture.
#[test]
fn no_two_insertable_conversions_bridge_the_same_pair_of_built_in_types() {
    register_media_builtin_node_types();
    let insertable: Vec<PinConversion> = NODE_REGISTRY
        .list_registered()
        .into_iter()
        .filter_map(|descriptor| {
            descriptor
                .pin_conversion()
                .unwrap_or_else(|refusal| panic!("{refusal}"))
        })
        .filter(PinConversion::may_be_inserted)
        .collect();
    assert!(!insertable.is_empty());
    let end_types = every_built_in_end_type(&insertable);

    for output in &end_types {
        for input in &end_types {
            let bridging: Vec<&NodeClassImportPath> = insertable
                .iter()
                .filter(|conversion| {
                    matches!(
                        the_verdict_between(output, input, std::slice::from_ref(conversion))
                            .outcome,
                        PinLinkOutcome::ConnectsThroughConvertNode(_)
                    )
                })
                .map(|conversion| &conversion.node_type)
                .collect();
            assert!(
                bridging.len() <= 1,
                "{output} into {input} is bridged by {bridging:?}"
            );
        }
    }
}
