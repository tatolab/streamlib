// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node catalog of a runtime built with the media built-ins lists each conversion that has a
//! typed form under the pin types it converts between, with typed pins and its conversion
//! stated, so the link verdict finds it between two ends.

use streamlib::sdk::descriptors::{
    NodeClassImportPath, NodeDescriptor, PinConversion, PinLinkEnd, PinLinkInputEnd,
    PinLinkOutcome, PinLinkVerdict, PinType, TexturePixelFormat, pin_link_verdict,
};
use streamlib::sdk::nodes::NODE_REGISTRY;
use streamlib_media_builtins::register_media_builtin_node_types;

/// The formats one `[h, w, 4]` tensor holds a pixel of, which `TextureToTensor` takes and
/// `TensorToTexture` writes.
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
fn the_typed_conversions() -> [PinConversion; 4] {
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
            a_texture_of(&ONE_TENSOR_PIXEL_FORMATS),
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

/// An `nv12` Texture has no one-tensor layout, so no conversion is offered into a Tensor, and
/// no Tensor reaches a Texture input that takes `nv12` alone.
#[test]
fn the_verdict_offers_no_conversion_to_or_from_an_nv12_texture() {
    let conversions = the_conversions_stated_by(&the_registered_built_ins());
    let nv12 = a_texture_of(&[TexturePixelFormat::Nv12]);

    for (output, input) in [(&nv12, &PinType::Tensor {}), (&PinType::Tensor {}, &nv12)] {
        let verdict = the_verdict_between(output, input, &conversions);
        assert!(
            matches!(verdict.outcome, PinLinkOutcome::Refused(_)),
            "{output} into {input}: {verdict:?}"
        );
    }
}

fn the_verdict_between(
    output_pin_type: &PinType,
    input_pin_type: &PinType,
    conversions: &[PinConversion],
) -> PinLinkVerdict {
    pin_link_verdict(
        PinLinkEnd {
            node_name: "upstream",
            port_name: "out",
            member_name: None,
            pin_type: output_pin_type,
            meaning: None,
        },
        PinLinkInputEnd {
            end: PinLinkEnd {
                node_name: "downstream",
                port_name: "in",
                member_name: None,
                pin_type: input_pin_type,
                meaning: None,
            },
            input_mode: None,
        },
        conversions,
    )
}
