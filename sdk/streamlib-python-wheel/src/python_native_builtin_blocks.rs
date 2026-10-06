// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel-exported names for the native media built-ins.
//!
//! `streamlib.TestPatternSource` is a marker class: never instantiated, never
//! subclassed, carrying no Python behavior. Its `type` class attribute is the
//! import path a graph names the statically-linked native processor by, which
//! `stream.add` records on the node — per-frame paths never enter the
//! interpreter.

/// Declare marker classes, each standing for one native processor, and the
/// function adding them to the module.
///
/// No marker has a `#[new]`: instantiating one is always a mistake, and PyO3's
/// "no constructor defined" error says so.
macro_rules! native_processor_marker_classes {
    (
        added_to_the_module_by: $add_marker_classes_to_the_module:ident,
        markers: [$(
            $(#[$marker_attribute:meta])*
            $marker:ident as $python_class_name:literal {
                $(dunder_test: $dunder_test:literal,)?
                import_path: $import_path:expr,
            }
        )+]
    ) => {
        $(
            $(#[$marker_attribute])*
            #[::pyo3::pyclass(name = $python_class_name, module = "streamlib", frozen)]
            pub(crate) struct $marker;

            #[::pyo3::pymethods]
            impl $marker {
                $(
                    /// pytest collects `Test*` classes by name; this tells it not to.
                    #[classattr]
                    #[pyo3(name = "__test__")]
                    fn dunder_test() -> bool {
                        $dunder_test
                    }
                )?

                /// The import path a graph names this marker's native processor by — on
                /// every floor, including one where that processor is not compiled in.
                #[classattr]
                #[pyo3(name = "type")]
                fn native_processor_class_import_path() -> String {
                    let native_processor_class_import_path:
                        ::streamlib::sdk::descriptors::ProcessorClassImportPath = $import_path;
                    native_processor_class_import_path.as_str().to_owned()
                }
            }
        )+

        /// Add every marker class this list declares to the module.
        pub(crate) fn $add_marker_classes_to_the_module(
            module: &::pyo3::Bound<'_, ::pyo3::types::PyModule>,
        ) -> ::pyo3::PyResult<()> {
            $(::pyo3::types::PyModuleMethods::add_class::<$marker>(module)?;)+
            Ok(())
        }
    };
}
pub(crate) use native_processor_marker_classes;

/// The import path `DisplayWindow` registers under where it is compiled in,
/// for a floor where it is not; held equal to the built-in's own derived path
/// by a test on Linux.
#[cfg(any(
    all(test, target_os = "linux"),
    not(any(target_os = "linux", target_os = "macos"))
))]
const DISPLAY_WINDOW_PROCESSOR_CLASS_IMPORT_PATH: &str =
    "streamlib_media_builtins::display_window::DisplayWindow";

native_processor_marker_classes! {
    added_to_the_module_by: add_native_builtin_marker_classes_to_the_module,
    markers: [
        /// `streamlib.TestPatternSource` — SMPTE-style color bars, no hardware.
        PythonTestPatternSourceBlock as "TestPatternSource" {
            dunder_test: false,
            import_path:
                streamlib_media_builtins::TestPatternSource::Processor::processor_class_import_path(),
        }
        /// `streamlib.CameraSource` — live camera capture through the engine's video
        /// device seam (V4L2 on Linux; a platform with no capture backend refuses at
        /// `setup()`).
        PythonCameraSourceBlock as "CameraSource" {
            import_path:
                streamlib_media_builtins::CameraSource::Processor::processor_class_import_path(),
        }
        /// `streamlib.DisplayWindow` — video frames in a vsync'd window.
        PythonDisplayWindowBlock as "DisplayWindow" {
            import_path: {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                {
                    streamlib_media_builtins::DisplayWindow::Processor::processor_class_import_path()
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                {
                    streamlib::sdk::descriptors::ProcessorClassImportPath::new(
                        DISPLAY_WINDOW_PROCESSOR_CLASS_IMPORT_PATH,
                    )
                    .expect("a non-blank literal is a valid import path")
                }
            },
        }
        /// `streamlib.MicrophoneSource` — audio capture on whichever backend the
        /// chain probed, silence where none exists.
        PythonMicrophoneSourceBlock as "MicrophoneSource" {
            import_path:
                streamlib_media_builtins::MicrophoneSource::Processor::processor_class_import_path(),
        }
        /// `streamlib.SpeakerSink` — audio playback on whichever backend the chain
        /// probed, discarding where none exists.
        PythonSpeakerSinkBlock as "SpeakerSink" {
            import_path:
                streamlib_media_builtins::SpeakerSink::Processor::processor_class_import_path(),
        }
        /// `streamlib.H264Encoder` — video frames to H.264 encoded-frame bags via
        /// hardware encode, on the platform's video codec arm.
        PythonH264EncoderBlock as "H264Encoder" {
            import_path:
                streamlib_media_builtins::H264Encoder::Processor::processor_class_import_path(),
        }
        /// `streamlib.H264Decoder` — H.264 encoded-frame bags to decoded video
        /// frames via hardware decode, on the platform's video codec arm.
        PythonH264DecoderBlock as "H264Decoder" {
            import_path:
                streamlib_media_builtins::H264Decoder::Processor::processor_class_import_path(),
        }
        /// `streamlib.H265Encoder` — video frames to H.265 encoded-frame bags via
        /// hardware encode, on the platform's video codec arm.
        PythonH265EncoderBlock as "H265Encoder" {
            import_path:
                streamlib_media_builtins::H265Encoder::Processor::processor_class_import_path(),
        }
        /// `streamlib.H265Decoder` — H.265 encoded-frame bags to decoded video
        /// frames via hardware decode, on the platform's video codec arm.
        PythonH265DecoderBlock as "H265Decoder" {
            import_path:
                streamlib_media_builtins::H265Decoder::Processor::processor_class_import_path(),
        }
        /// `streamlib.OpusEncoder` — 20 ms windows of audio to Opus
        /// encoded-audio-packet bags via statically linked libopus, on every
        /// platform the wheel builds for.
        PythonOpusEncoderBlock as "OpusEncoder" {
            import_path:
                streamlib_media_builtins::OpusEncoder::Processor::processor_class_import_path(),
        }
        /// `streamlib.OpusDecoder` — Opus encoded-audio-packet bags to decoded
        /// audio blocks via statically linked libopus, on every platform the wheel
        /// builds for.
        PythonOpusDecoderBlock as "OpusDecoder" {
            import_path:
                streamlib_media_builtins::OpusDecoder::Processor::processor_class_import_path(),
        }
        /// `streamlib.Mp4Sink` — encoded video and audio bags recorded to one
        /// fragmented MP4 file, one track per inbound link, on every platform the
        /// wheel builds for.
        PythonMp4SinkBlock as "Mp4Sink" {
            import_path:
                streamlib_media_builtins::Mp4Sink::Processor::processor_class_import_path(),
        }
        /// `streamlib.VirtualCameraSink` — video frames presented as a virtual
        /// camera any Linux application can select (Linux).
        PythonVirtualCameraSinkBlock as "VirtualCameraSink" {
            import_path: {
                #[cfg(target_os = "linux")]
                {
                    streamlib_media_builtins::VirtualCameraSink::Processor::processor_class_import_path()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    streamlib::sdk::descriptors::ProcessorClassImportPath::new(
                        streamlib_api_server::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
                    )
                    .expect("a non-blank literal is a valid import path")
                }
            },
        }
    ]
}

/// Register the native built-in processor types on the process-wide registry.
/// Idempotent; called once at module import.
pub(crate) fn register_native_builtin_processor_types() {
    streamlib_media_builtins::register_media_builtin_processor_types();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use pyo3::prelude::*;
    use pyo3::types::{PyModule, PyType};
    use streamlib::sdk::descriptors::ProcessorClassImportPath;
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;

    use super::*;

    /// The control plane's virtual-camera prompt names the built-in by a path
    /// it cannot derive, because the api-server does not link the media
    /// built-ins; the wheel links both, so this is where the two must agree.
    #[test]
    fn the_virtual_camera_prompt_names_the_path_the_built_in_registers_under() {
        assert_eq!(
            streamlib_api_server::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
            streamlib_media_builtins::VirtualCameraSink::Processor::processor_class_import_path()
                .as_str()
        );
    }

    #[test]
    fn the_display_window_path_for_a_floor_without_it_is_the_one_it_registers_under() {
        assert_eq!(
            DISPLAY_WINDOW_PROCESSOR_CLASS_IMPORT_PATH,
            streamlib_media_builtins::DisplayWindow::Processor::processor_class_import_path()
                .as_str()
        );
    }

    /// A graph names a native node by its marker's `type`, so every marker the
    /// module exports must name the path its native processor registered under.
    #[test]
    fn every_exported_marker_type_names_a_registered_native_processor() {
        Python::initialize();
        register_native_builtin_processor_types();
        Python::attach(|python| {
            let module = PyModule::new(python, "native_builtin_markers_under_test").unwrap();
            add_native_builtin_marker_classes_to_the_module(&module).unwrap();
            let marker_classes: Vec<_> = module
                .dict()
                .values()
                .into_iter()
                .filter(|exported| exported.is_instance_of::<PyType>())
                .collect();
            assert!(!marker_classes.is_empty());
            for marker_class in marker_classes {
                let type_attribute: String =
                    marker_class.getattr("type").unwrap().extract().unwrap();
                assert!(
                    type_attribute.starts_with("streamlib_media_builtins::"),
                    "{marker_class}: {type_attribute}"
                );
                assert!(
                    PROCESSOR_REGISTRY.is_registered(
                        &ProcessorClassImportPath::new(type_attribute.as_str()).unwrap()
                    ),
                    "{marker_class}: {type_attribute}"
                );
            }
        });
    }
}
