// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel-exported names for the native media built-ins.
//!
//! `streamlib.TestPatternSource` is a marker class: never instantiated, never
//! subclassed, carrying no Python behavior. `Runtime.add` recognizes the type
//! object itself and resolves it to the statically-linked native processor —
//! per-frame paths never enter the interpreter.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::type_object::PyTypeInfo;
use streamlib::sdk::descriptors::ProcessorClassImportPath;

/// A wheel-exported marker class standing for one native processor.
pub(crate) trait NativeProcessorMarkerClass: PyTypeInfo {
    /// The import path this marker's native processor registers under — on
    /// every floor, including one where that processor is not compiled in.
    fn native_processor_class_import_path() -> ProcessorClassImportPath;

    /// Why `Runtime.add` refuses this marker on this floor, which does not
    /// compile its native processor in.
    fn refusal_on_this_floor() -> Option<&'static str> {
        None
    }

    /// This marker's import path, or its refusal on this floor, when
    /// `candidate_class` is this marker's type object itself.
    fn native_processor_class_import_path_if_it_is(
        python: Python<'_>,
        candidate_class: &Bound<'_, PyAny>,
    ) -> Option<PyResult<ProcessorClassImportPath>> {
        if !candidate_class.is(python.get_type::<Self>()) {
            return None;
        }
        Some(match Self::refusal_on_this_floor() {
            Some(refusal_on_this_floor) => Err(PyRuntimeError::new_err(refusal_on_this_floor)),
            None => Ok(Self::native_processor_class_import_path()),
        })
    }
}

/// How one marker answers whether a class is it, and with which import path.
pub(crate) type NativeProcessorClassImportPathIfItIs =
    fn(Python<'_>, &Bound<'_, PyAny>) -> Option<PyResult<ProcessorClassImportPath>>;

/// The value of a marker's `type` class attribute: the import path a graph
/// names the marker's native processor by.
pub(crate) fn marker_type_class_attribute<Marker: NativeProcessorMarkerClass>() -> String {
    Marker::native_processor_class_import_path()
        .as_str()
        .to_owned()
}

/// Declare marker classes, each standing for one native processor, with the
/// resolver list `Runtime.add` searches and the function adding them to the
/// module.
///
/// No marker has a `#[new]`: instantiating one is always a mistake, and PyO3's
/// "no constructor defined" error says so.
macro_rules! native_processor_marker_classes {
    (
        resolvers: $marker_import_path_resolvers:ident,
        added_to_the_module_by: $add_marker_classes_to_the_module:ident,
        markers: [$(
            $(#[$marker_attribute:meta])*
            $marker:ident as $python_class_name:literal {
                $(dunder_test: $dunder_test:literal,)?
                import_path: $import_path:expr,
                $(#[$floor_refusing_the_marker:meta] refused_on_this_floor: $refusal_on_this_floor:literal,)?
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

                #[classattr]
                #[pyo3(name = "type")]
                fn native_processor_type() -> String {
                    $crate::python_native_builtin_blocks::marker_type_class_attribute::<Self>()
                }
            }

            impl $crate::python_native_builtin_blocks::NativeProcessorMarkerClass for $marker {
                fn native_processor_class_import_path(
                ) -> ::streamlib::sdk::descriptors::ProcessorClassImportPath {
                    $import_path
                }

                $(
                    #[$floor_refusing_the_marker]
                    fn refusal_on_this_floor() -> Option<&'static str> {
                        Some($refusal_on_this_floor)
                    }
                )?
            }
        )+

        /// Every marker this list declares, by its import-path resolver.
        const $marker_import_path_resolvers: &[
            $crate::python_native_builtin_blocks::NativeProcessorClassImportPathIfItIs
        ] = &[$(
            <$marker as $crate::python_native_builtin_blocks::NativeProcessorMarkerClass>
                ::native_processor_class_import_path_if_it_is,
        )+];

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
    resolvers: NATIVE_BUILTIN_MARKER_IMPORT_PATH_RESOLVERS,
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
                    ProcessorClassImportPath::new(DISPLAY_WINDOW_PROCESSOR_CLASS_IMPORT_PATH)
                        .expect("a non-blank literal is a valid import path")
                }
            },
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            refused_on_this_floor: "DisplayWindow runs on Linux and macOS; this platform is not \
                                    supported by the streamlib wheel yet",
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
                    ProcessorClassImportPath::new(
                        streamlib_api_server::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
                    )
                    .expect("a non-blank literal is a valid import path")
                }
            },
            #[cfg(not(target_os = "linux"))]
            refused_on_this_floor: "VirtualCameraSink is Linux-only today; this platform is not \
                                    supported by the streamlib wheel yet",
        }
    ]
}

/// Resolve a Python object to a native built-in's class import path, if it is
/// one of the wheel-exported marker type objects. On a platform where a
/// marker's native processor is not compiled in, the answer is an explicit
/// unsupported-platform error rather than the generic "not a processor"
/// rejection.
pub(crate) fn native_builtin_class_import_path(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<Option<ProcessorClassImportPath>> {
    NATIVE_BUILTIN_MARKER_IMPORT_PATH_RESOLVERS
        .iter()
        .find_map(|import_path_if_it_is| import_path_if_it_is(python, processor_class))
        .transpose()
}

/// Register the native built-in processor types on the process-wide registry.
/// Idempotent; called once at module import.
pub(crate) fn register_native_builtin_processor_types() {
    streamlib_media_builtins::register_media_builtin_processor_types();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
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

    #[test]
    fn every_marker_type_attribute_is_the_path_add_resolves_the_marker_to() {
        Python::initialize();
        Python::attach(|python| {
            let marker_classes = [
                python.get_type::<PythonTestPatternSourceBlock>(),
                python.get_type::<PythonCameraSourceBlock>(),
                python.get_type::<PythonDisplayWindowBlock>(),
                python.get_type::<PythonMicrophoneSourceBlock>(),
                python.get_type::<PythonSpeakerSinkBlock>(),
                python.get_type::<PythonH264EncoderBlock>(),
                python.get_type::<PythonH264DecoderBlock>(),
                python.get_type::<PythonH265EncoderBlock>(),
                python.get_type::<PythonH265DecoderBlock>(),
                python.get_type::<PythonOpusEncoderBlock>(),
                python.get_type::<PythonOpusDecoderBlock>(),
                python.get_type::<PythonMp4SinkBlock>(),
                python.get_type::<PythonVirtualCameraSinkBlock>(),
            ];
            assert_eq!(
                marker_classes.len(),
                NATIVE_BUILTIN_MARKER_IMPORT_PATH_RESOLVERS.len()
            );
            for marker_class in marker_classes {
                let type_attribute: String =
                    marker_class.getattr("type").unwrap().extract().unwrap();
                let resolved = native_builtin_class_import_path(python, marker_class.as_any())
                    .unwrap()
                    .unwrap();
                assert_eq!(type_attribute, resolved.as_str(), "{marker_class}");
                assert!(
                    type_attribute.starts_with("streamlib_media_builtins::"),
                    "{marker_class}: {type_attribute}"
                );
            }
        });
    }
}
