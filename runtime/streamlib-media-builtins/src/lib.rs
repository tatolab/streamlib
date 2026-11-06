// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! First-party media built-ins: native pre-built blocks statically linked
//! into the wheel and instantiated from Python by configuration
//! (`stream_builder.add(RelayTrackSource)`), whose per-frame paths never enter the
//! interpreter.
//!
//! Written against the SDK's handle-shaped primitives only — pixel-buffer
//! pool, texture cache — never private engine guts.

pub mod audio_block;
pub mod h273_color_vui_translation;
#[cfg(test)]
mod msgpack_wire_test_support;
pub mod port_family_conversion;
pub(crate) mod processor_thread_join;
pub mod relay_track_source;
pub mod video_frame;

pub use audio_block::{AudioBlock, AudioSampleDtype};
pub use port_family_conversion::{
    ArrayToChannels, ArrayToChannelsConfig, ChannelsToArray, ChannelsToRecords,
    ChannelsToRecordsConfig, RecordsToChannels, RecordsToChannelsConfig,
};
pub use relay_track_source::{RelayTrackSource, RelayTrackSourceConfig};
pub use video_frame::VideoFrame;

use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;

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
];

/// Register every media built-in on the process-wide registry, and record each
/// one that moved to a module, so a graph naming it is refused naming the
/// module to install. In-process static registration (the api-server
/// precedent) — no dlopen, and idempotent, so hosts may call it more than once.
pub fn register_media_builtin_processor_types() {
    PROCESSOR_REGISTRY.register::<relay_track_source::RelayTrackSource::Processor>();
    PROCESSOR_REGISTRY.register::<port_family_conversion::ChannelsToArray::Processor>();
    PROCESSOR_REGISTRY.register::<port_family_conversion::ArrayToChannels::Processor>();
    PROCESSOR_REGISTRY.register::<port_family_conversion::ChannelsToRecords::Processor>();
    PROCESSOR_REGISTRY.register::<port_family_conversion::RecordsToChannels::Processor>();

    for (former_class_name, module_node_type, module_distribution) in BUILT_INS_MOVED_TO_MODULES {
        match ProcessorClassImportPath::new(*module_node_type) {
            Ok(module_node_type) => PROCESSOR_REGISTRY
                .register_built_in_node_type_moved_to_a_module(
                    ProcessorClassImportPath::of_built_in_node(former_class_name),
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

#[cfg(test)]
mod built_in_node_type_tests {
    use streamlib::sdk::error::Error;

    use super::*;

    /// Every built-in this floor registers, each held to its class in
    /// `tatolab.stream` — its authored name, which the descriptor carries.
    fn every_built_in_registered_on_this_floor() -> Vec<ProcessorClassImportPath> {
        register_media_builtin_processor_types();
        let built_ins: Vec<ProcessorClassImportPath> = PROCESSOR_REGISTRY
            .list_registered()
            .into_iter()
            .filter(|descriptor| {
                descriptor
                    .processor_class_import_path
                    .names_a_built_in_node()
            })
            .map(|descriptor| {
                assert_eq!(
                    descriptor.processor_class_import_path,
                    ProcessorClassImportPath::of_built_in_node(
                        descriptor.processor_class_short_name.as_str()
                    ),
                    "a built-in's type is its authored name in `tatolab.stream`"
                );
                descriptor.processor_class_import_path
            })
            .collect();
        assert!(!built_ins.is_empty());
        built_ins
    }

    #[test]
    fn every_built_in_is_registered_as_its_authored_name_in_the_stream_package() {
        let built_ins = every_built_in_registered_on_this_floor();

        assert!(
            built_ins
                .contains(&relay_track_source::RelayTrackSource::processor_class_import_path())
        );
        let rust_module_paths: Vec<ProcessorClassImportPath> = PROCESSOR_REGISTRY
            .registered_processor_class_import_paths()
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
            let refusal = PROCESSOR_REGISTRY.refuse_a_node_this_runtime_cannot_add(
                "probe",
                &built_in_node_type,
                &serde_json::json!({"a_setting_no_built_in_takes": 1}),
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
        register_media_builtin_processor_types();

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
        ] {
            let former_node_type = ProcessorClassImportPath::of_built_in_node(former_class_name);
            assert!(!PROCESSOR_REGISTRY.is_registered(&former_node_type));

            let refusal = PROCESSOR_REGISTRY
                .refuse_a_node_this_runtime_cannot_add(
                    "former",
                    &former_node_type,
                    &serde_json::json!({"device_id": "default"}),
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
