// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! First-party media built-ins: native pre-built blocks statically linked
//! into the wheel and instantiated from Python by configuration
//! (`stream_builder.add(AudioToTensor)`), whose per-frame paths never enter the
//! interpreter.
//!
//! Written against the SDK's handle-shaped primitives only — pixel-buffer
//! pool, texture cache — never private engine guts.

pub mod audio_block;
pub mod bundle_members;
#[cfg(test)]
mod bundle_members_tests;
pub mod h273_color_vui_translation;
#[cfg(test)]
mod msgpack_wire_test_support;
pub mod pin_type_conversion;
pub mod video_frame;

pub use audio_block::{AudioBlock, AudioSampleDtype};
pub use bundle_members::{BreakBundle, MakeBundle};
pub use pin_type_conversion::{
    AudioToTensor, TensorToAudio, TensorToAudioInputs, TensorToTexture, TextureToTensor,
};
pub use video_frame::VideoFrame;

use streamlib::sdk::descriptors::{NodeClassImportPath, PinConversionOutputPick};
use streamlib::sdk::nodes::NODE_REGISTRY;

/// Each built-in that left the runtime for a module, by its former public class
/// name, with the module's node type and the distribution that installs it.
const BUILT_INS_MOVED_TO_MODULES: &[(&str, &str, &str)] = &[
    (
        "CameraSource",
        "tatolab.camera:CameraSource",
        "tatolab-camera",
    ),
    (
        "MicrophoneSource",
        "tatolab.microphone:MicrophoneSource",
        "tatolab-microphone",
    ),
    (
        "SpeakerSink",
        "tatolab.speaker:SpeakerSink",
        "tatolab-speaker",
    ),
    ("H264Encoder", "tatolab.h264:H264Encoder", "tatolab-h264"),
    ("H264Decoder", "tatolab.h264:H264Decoder", "tatolab-h264"),
    ("H265Encoder", "tatolab.h265:H265Encoder", "tatolab-h265"),
    ("H265Decoder", "tatolab.h265:H265Decoder", "tatolab-h265"),
    ("OpusEncoder", "tatolab.opus:OpusEncoder", "tatolab-opus"),
    ("OpusDecoder", "tatolab.opus:OpusDecoder", "tatolab-opus"),
    (
        "TestPatternSource",
        "tatolab.testpattern:TestPatternSource",
        "tatolab-testpattern",
    ),
    (
        "VirtualCameraSink",
        "tatolab.virtualcamera:VirtualCameraSink",
        "tatolab-virtualcamera",
    ),
    (
        "RelayTrackSource",
        "tatolab.moq:MoqTrackReceive",
        "tatolab-moq",
    ),
];

/// Register every media built-in on the process-wide registry, and record each
/// one that moved to a module, so a graph naming it is refused naming the
/// module to install. In-process static registration (the api-server
/// precedent) — no dlopen, and idempotent, so hosts may call it more than once.
pub fn register_media_builtin_node_types() {
    register_the_conversion_node_types();
    NODE_REGISTRY.register::<MakeBundle::Node>();
    NODE_REGISTRY.register::<BreakBundle::Node>();

    for (former_class_name, module_node_type, module_distribution) in BUILT_INS_MOVED_TO_MODULES {
        match NodeClassImportPath::new(*module_node_type) {
            Ok(module_node_type) => NODE_REGISTRY.register_built_in_node_type_moved_to_a_module(
                NodeClassImportPath::of_built_in_node(former_class_name),
                module_node_type,
                *module_distribution,
            ),
            Err(refusal) => tracing::error!(
                former_class_name,
                module_node_type,
                %refusal,
                "a moved built-in's module node type names no class, so its former type is \
                 refused as unknown"
            ),
        }
    }
}

/// Register each built-in conversion node, each with the function that picks the setting its
/// output follows when it has one, and each number conversion with the cast it makes of a saved
/// value, on the process-wide registry.
pub fn register_the_conversion_node_types() {
    use pin_type_conversion::number_conversions::{
        BooleanToFloat, BooleanToInteger, FloatToBoolean, FloatToInteger, IntegerToBoolean,
        IntegerToFloat, the_number_cast_of_a_saved_value,
    };
    let the_number_cast = the_number_cast_of_a_saved_value();
    NODE_REGISTRY.register_a_conversion_casting_a_saved_value::<FloatToInteger::Node>(
        the_number_cast.clone(),
    );
    NODE_REGISTRY.register_a_conversion_casting_a_saved_value::<IntegerToFloat::Node>(
        the_number_cast.clone(),
    );
    NODE_REGISTRY.register_a_conversion_casting_a_saved_value::<BooleanToInteger::Node>(
        the_number_cast.clone(),
    );
    NODE_REGISTRY.register_a_conversion_casting_a_saved_value::<IntegerToBoolean::Node>(
        the_number_cast.clone(),
    );
    NODE_REGISTRY.register_a_conversion_casting_a_saved_value::<BooleanToFloat::Node>(
        the_number_cast.clone(),
    );
    NODE_REGISTRY
        .register_a_conversion_casting_a_saved_value::<FloatToBoolean::Node>(the_number_cast);
    NODE_REGISTRY.register::<pin_type_conversion::AudioToTensor::Node>();
    NODE_REGISTRY.register::<pin_type_conversion::TensorToAudio::Node>();
    NODE_REGISTRY.register::<pin_type_conversion::TextureToTensor::Node>();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    NODE_REGISTRY.register::<pin_type_conversion::TextureFormatConvert::Node>();
    NODE_REGISTRY.register_a_conversion_whose_output_a_setting_picks::<
        pin_type_conversion::TensorToTexture::Node,
    >(PinConversionOutputPick {
        picked_for: pin_type_conversion::tensor_to_texture::the_format_picked_for_the_input,
        given_under: pin_type_conversion::tensor_to_texture::the_texture_written_under_the_settings,
    });
}

#[cfg(test)]
mod built_in_node_type_tests {
    use streamlib::sdk::error::Error;

    use super::*;

    /// Every built-in this floor registers, each held to its class in
    /// `tatolab.stream` — its authored name, which the descriptor carries.
    fn every_built_in_registered_on_this_floor() -> Vec<NodeClassImportPath> {
        register_media_builtin_node_types();
        let built_ins: Vec<NodeClassImportPath> = NODE_REGISTRY
            .list_registered()
            .into_iter()
            .filter(|descriptor| descriptor.node_class_import_path.names_a_built_in_node())
            .map(|descriptor| {
                assert_eq!(
                    descriptor.node_class_import_path,
                    NodeClassImportPath::of_built_in_node(
                        descriptor.node_class_short_name.as_str()
                    ),
                    "a built-in's type is its authored name in `tatolab.stream`"
                );
                descriptor.node_class_import_path
            })
            .collect();
        assert!(!built_ins.is_empty());
        built_ins
    }

    #[test]
    fn every_built_in_is_registered_as_its_authored_name_in_the_stream_package() {
        let built_ins = every_built_in_registered_on_this_floor();

        assert!(built_ins.contains(&pin_type_conversion::AudioToTensor::node_class_import_path()));
        let rust_module_paths: Vec<NodeClassImportPath> = NODE_REGISTRY
            .registered_node_class_import_paths()
            .into_iter()
            .filter(|path| path.as_str().starts_with("streamlib_media_builtins::"))
            .collect();
        assert!(
            rust_module_paths.is_empty(),
            "a built-in's Rust module path reached the registry: {rust_module_paths:?}"
        );
    }

    /// Each built-in's config derives `deny_unknown_fields` by hand, so this is
    /// what fails when one does not — any later built-in included.
    #[test]
    fn every_built_in_refuses_a_setting_it_does_not_take_naming_it() {
        for built_in_node_type in every_built_in_registered_on_this_floor() {
            let refusal = NODE_REGISTRY.refuse_a_node_this_runtime_cannot_add(
                "probe",
                &built_in_node_type,
                serde_json::json!({"a_setting_no_built_in_takes": 1})
                    .as_object()
                    .expect("the probe is an object of input values"),
            );

            match refusal {
                Err(Error::NodeConfigRefused { refusal, .. }) => assert!(
                    refusal.contains("`a_setting_no_built_in_takes`"),
                    "{built_in_node_type}: {refusal}"
                ),
                other => panic!("{built_in_node_type} took a setting it does not know: {other:?}"),
            }
        }
    }

    /// A graph recorded before a built-in moved to a module names its former
    /// type, and is refused naming the module's type and what to install.
    #[test]
    fn the_former_media_built_ins_are_refused_naming_the_module_to_install() {
        register_media_builtin_node_types();

        for (former_class_name, module_node_type, module_distribution) in [
            (
                "CameraSource",
                "tatolab.camera:CameraSource",
                "tatolab-camera",
            ),
            (
                "MicrophoneSource",
                "tatolab.microphone:MicrophoneSource",
                "tatolab-microphone",
            ),
            (
                "SpeakerSink",
                "tatolab.speaker:SpeakerSink",
                "tatolab-speaker",
            ),
            ("OpusEncoder", "tatolab.opus:OpusEncoder", "tatolab-opus"),
            ("OpusDecoder", "tatolab.opus:OpusDecoder", "tatolab-opus"),
            ("H264Encoder", "tatolab.h264:H264Encoder", "tatolab-h264"),
            ("H264Decoder", "tatolab.h264:H264Decoder", "tatolab-h264"),
            ("H265Encoder", "tatolab.h265:H265Encoder", "tatolab-h265"),
            ("H265Decoder", "tatolab.h265:H265Decoder", "tatolab-h265"),
            (
                "TestPatternSource",
                "tatolab.testpattern:TestPatternSource",
                "tatolab-testpattern",
            ),
            (
                "VirtualCameraSink",
                "tatolab.virtualcamera:VirtualCameraSink",
                "tatolab-virtualcamera",
            ),
            (
                "RelayTrackSource",
                "tatolab.moq:MoqTrackReceive",
                "tatolab-moq",
            ),
        ] {
            let former_node_type = NodeClassImportPath::of_built_in_node(former_class_name);
            assert!(!NODE_REGISTRY.is_registered(&former_node_type));

            let refusal = NODE_REGISTRY
                .refuse_a_node_this_runtime_cannot_add(
                    "former",
                    &former_node_type,
                    serde_json::json!({"device_id": "default"})
                        .as_object()
                        .expect("the probe is an object of input values"),
                )
                .unwrap_err();

            match &refusal {
                Error::BuiltInNodeTypeMovedToAModule {
                    node_type,
                    module_node_type: refused_naming,
                    module_distribution: refused_distribution,
                } => {
                    assert_eq!(*node_type, former_node_type);
                    assert_eq!(refused_naming.as_str(), module_node_type);
                    assert_eq!(refused_distribution, module_distribution);
                }
                other => panic!(
                    "{former_node_type}: expected the moved-to-a-module refusal, got {other:?}"
                ),
            }
            let message = refusal.to_string();
            assert!(message.contains(module_node_type), "{message}");
            assert!(
                message.contains(&format!("pip install {module_distribution}")),
                "{message}"
            );
        }
    }
}
