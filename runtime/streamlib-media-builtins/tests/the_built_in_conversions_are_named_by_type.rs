// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node catalog of a runtime built with the media built-ins lists each conversion that has a
//! typed form under the pin types it converts between, with typed pins and its conversion
//! stated, so the link verdict finds it between two ends.

use streamlib::sdk::descriptors::{
    NodeClassImportPath, NodeDescriptor, PinConversion, PinLinkEnd, PinLinkInputEnd,
    PinLinkOutcome, PinType, pin_link_verdict,
};
use streamlib::sdk::nodes::NODE_REGISTRY;
use streamlib_media_builtins::register_media_builtin_node_types;

fn texture() -> PinType {
    PinType::Texture {
        pixel_formats: Vec::new(),
    }
}

/// Each built-in conversion with a typed form: its name, its wired input and the type it takes,
/// and its output and the type it gives.
fn the_typed_conversions() -> [(&'static str, &'static str, PinType, &'static str, PinType); 4] {
    [
        (
            "TextureToTensor",
            "image",
            texture(),
            "array",
            PinType::Tensor {},
        ),
        (
            "TensorToTexture",
            "array",
            PinType::Tensor {},
            "image",
            texture(),
        ),
        (
            "AudioToTensor",
            "channels",
            PinType::Audio {},
            "array",
            PinType::Tensor {},
        ),
        (
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

    for (name, input_port_name, input_pin_type, output_port_name, output_pin_type) in
        the_typed_conversions()
    {
        let node_type = NodeClassImportPath::of_built_in_node(name);
        let descriptor = built_ins
            .iter()
            .find(|descriptor| descriptor.node_class_import_path == node_type)
            .unwrap_or_else(|| panic!("the catalog lists no {node_type}"));
        assert!(descriptor.conversion, "{node_type} states no conversion");
        assert_eq!(
            conversions
                .iter()
                .find(|conversion| conversion.node_type == node_type),
            Some(&PinConversion {
                node_type: node_type.clone(),
                input_port_name: input_port_name.to_string(),
                input_pin_type,
                output_port_name: output_port_name.to_string(),
                output_pin_type,
            }),
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

    for (name, _, input_pin_type, _, output_pin_type) in the_typed_conversions() {
        let verdict = pin_link_verdict(
            PinLinkEnd {
                node_name: "upstream",
                port_name: "out",
                member_name: None,
                pin_type: &input_pin_type,
                meaning: None,
            },
            PinLinkInputEnd {
                end: PinLinkEnd {
                    node_name: "downstream",
                    port_name: "in",
                    member_name: None,
                    pin_type: &output_pin_type,
                    meaning: None,
                },
                input_mode: None,
            },
            &conversions,
        );

        match verdict.outcome {
            PinLinkOutcome::ConnectsThroughConvertNode(convert_node) => assert_eq!(
                convert_node.conversion.node_type,
                NodeClassImportPath::of_built_in_node(name)
            ),
            other => panic!("{input_pin_type} into {output_pin_type}: {other:?}"),
        }
    }
}
