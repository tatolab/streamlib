// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel-exported names for the native media built-ins.
//!
//! `streamlib.TestPatternSource` is a marker class: never instantiated, never
//! subclassed, carrying no Python behavior. `Runtime.add` recognizes the type
//! object itself and resolves it to the statically-linked native processor —
//! per-frame paths never enter the interpreter.

// Only the unsupported-platform arms below raise, and they compile away on
// Linux — where every marker resolves.
#[cfg(not(target_os = "linux"))]
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::type_object::PyTypeInfo;
use streamlib::sdk::descriptors::ProcessorClassImportPath;

/// A wheel-exported marker class standing for one native processor.
pub(crate) trait NativeProcessorMarkerClass: PyTypeInfo {
    /// The import path this marker's native processor registers under — on
    /// every floor, including one where that processor is not compiled in.
    fn native_processor_class_import_path() -> ProcessorClassImportPath;

    /// This marker's import path, when `candidate_class` is this marker's
    /// type object itself.
    fn native_processor_class_import_path_if_it_is(
        python: Python<'_>,
        candidate_class: &Bound<'_, PyAny>,
    ) -> Option<ProcessorClassImportPath> {
        candidate_class
            .is(python.get_type::<Self>())
            .then(Self::native_processor_class_import_path)
    }
}

/// The value of a marker's `type` class attribute: the import path a graph
/// names the marker's native processor by.
pub(crate) fn marker_type_class_attribute<Marker: NativeProcessorMarkerClass>() -> String {
    Marker::native_processor_class_import_path()
        .as_str()
        .to_owned()
}

/// `streamlib.TestPatternSource` — SMPTE-style color bars, no hardware.
///
/// No `#[new]`: instantiating a marker is always a mistake, and PyO3's
/// "no constructor defined" error says so.
#[pyclass(name = "TestPatternSource", module = "streamlib", frozen)]
pub(crate) struct PythonTestPatternSourceBlock;

#[pymethods]
impl PythonTestPatternSourceBlock {
    /// The class is named `Test*`, which pytest would otherwise collect as a
    /// test class; this attribute tells it not to.
    #[classattr]
    #[pyo3(name = "__test__")]
    fn dunder_test() -> bool {
        false
    }

    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonTestPatternSourceBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::TestPatternSource::Processor::processor_class_import_path()
    }
}

/// `streamlib.CameraSource` — live camera capture through the engine's video
/// device seam (V4L2 on Linux; a platform with no capture backend refuses at
/// `setup()`).
#[pyclass(name = "CameraSource", module = "streamlib", frozen)]
pub(crate) struct PythonCameraSourceBlock;

#[pymethods]
impl PythonCameraSourceBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonCameraSourceBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::CameraSource::Processor::processor_class_import_path()
    }
}

/// `streamlib.DisplayWindow` — video frames in a vsync'd window.
#[pyclass(name = "DisplayWindow", module = "streamlib", frozen)]
pub(crate) struct PythonDisplayWindowBlock;

#[pymethods]
impl PythonDisplayWindowBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

/// The import path `DisplayWindow` registers under where it is compiled in,
/// for a floor where it is not; held equal to the built-in's own derived path
/// by a test on Linux.
#[cfg(any(
    all(test, target_os = "linux"),
    not(any(target_os = "linux", target_os = "macos"))
))]
const DISPLAY_WINDOW_PROCESSOR_CLASS_IMPORT_PATH: &str =
    "streamlib_media_builtins::display_window::DisplayWindow";

impl NativeProcessorMarkerClass for PythonDisplayWindowBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return streamlib_media_builtins::DisplayWindow::Processor::processor_class_import_path();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return ProcessorClassImportPath::new(DISPLAY_WINDOW_PROCESSOR_CLASS_IMPORT_PATH)
            .expect("a non-blank literal is a valid import path");
    }
}

/// `streamlib.MicrophoneSource` — audio capture on whichever backend the
/// chain probed, silence where none exists.
#[pyclass(name = "MicrophoneSource", module = "streamlib", frozen)]
pub(crate) struct PythonMicrophoneSourceBlock;

#[pymethods]
impl PythonMicrophoneSourceBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonMicrophoneSourceBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::MicrophoneSource::Processor::processor_class_import_path()
    }
}

/// `streamlib.SpeakerSink` — audio playback on whichever backend the chain
/// probed, discarding where none exists.
#[pyclass(name = "SpeakerSink", module = "streamlib", frozen)]
pub(crate) struct PythonSpeakerSinkBlock;

#[pymethods]
impl PythonSpeakerSinkBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonSpeakerSinkBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::SpeakerSink::Processor::processor_class_import_path()
    }
}

/// `streamlib.H264Encoder` — video frames to H.264 encoded-frame bags via
/// hardware encode, on the platform's video codec arm.
#[pyclass(name = "H264Encoder", module = "streamlib", frozen)]
pub(crate) struct PythonH264EncoderBlock;

#[pymethods]
impl PythonH264EncoderBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonH264EncoderBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::H264Encoder::Processor::processor_class_import_path()
    }
}

/// `streamlib.H264Decoder` — H.264 encoded-frame bags to decoded video
/// frames via hardware decode, on the platform's video codec arm.
#[pyclass(name = "H264Decoder", module = "streamlib", frozen)]
pub(crate) struct PythonH264DecoderBlock;

#[pymethods]
impl PythonH264DecoderBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonH264DecoderBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::H264Decoder::Processor::processor_class_import_path()
    }
}

/// `streamlib.H265Encoder` — video frames to H.265 encoded-frame bags via
/// hardware encode, on the platform's video codec arm.
#[pyclass(name = "H265Encoder", module = "streamlib", frozen)]
pub(crate) struct PythonH265EncoderBlock;

#[pymethods]
impl PythonH265EncoderBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonH265EncoderBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::H265Encoder::Processor::processor_class_import_path()
    }
}

/// `streamlib.H265Decoder` — H.265 encoded-frame bags to decoded video
/// frames via hardware decode, on the platform's video codec arm.
#[pyclass(name = "H265Decoder", module = "streamlib", frozen)]
pub(crate) struct PythonH265DecoderBlock;

#[pymethods]
impl PythonH265DecoderBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonH265DecoderBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::H265Decoder::Processor::processor_class_import_path()
    }
}

/// `streamlib.OpusEncoder` — 20 ms windows of audio to Opus
/// encoded-audio-packet bags via statically linked libopus, on every
/// platform the wheel builds for.
#[pyclass(name = "OpusEncoder", module = "streamlib", frozen)]
pub(crate) struct PythonOpusEncoderBlock;

#[pymethods]
impl PythonOpusEncoderBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonOpusEncoderBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::OpusEncoder::Processor::processor_class_import_path()
    }
}

/// `streamlib.OpusDecoder` — Opus encoded-audio-packet bags to decoded
/// audio blocks via statically linked libopus, on every platform the wheel
/// builds for.
#[pyclass(name = "OpusDecoder", module = "streamlib", frozen)]
pub(crate) struct PythonOpusDecoderBlock;

#[pymethods]
impl PythonOpusDecoderBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonOpusDecoderBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::OpusDecoder::Processor::processor_class_import_path()
    }
}

/// `streamlib.Mp4Sink` — encoded video and audio bags recorded to one
/// fragmented MP4 file, one track per inbound link, on every platform the
/// wheel builds for.
#[pyclass(name = "Mp4Sink", module = "streamlib", frozen)]
pub(crate) struct PythonMp4SinkBlock;

#[pymethods]
impl PythonMp4SinkBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonMp4SinkBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        streamlib_media_builtins::Mp4Sink::Processor::processor_class_import_path()
    }
}

/// `streamlib.VirtualCameraSink` — video frames presented as a virtual
/// camera any Linux application can select (Linux).
#[pyclass(name = "VirtualCameraSink", module = "streamlib", frozen)]
pub(crate) struct PythonVirtualCameraSinkBlock;

#[pymethods]
impl PythonVirtualCameraSinkBlock {
    #[classattr]
    #[pyo3(name = "type")]
    fn native_processor_type() -> String {
        marker_type_class_attribute::<Self>()
    }
}

impl NativeProcessorMarkerClass for PythonVirtualCameraSinkBlock {
    fn native_processor_class_import_path() -> ProcessorClassImportPath {
        #[cfg(target_os = "linux")]
        return streamlib_media_builtins::VirtualCameraSink::Processor::processor_class_import_path(
        );
        #[cfg(not(target_os = "linux"))]
        return ProcessorClassImportPath::new(
            streamlib_api_server::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
        )
        .expect("a non-blank literal is a valid import path");
    }
}

/// How one marker answers whether a class is it, and with which import path.
type NativeProcessorClassImportPathIfItIs =
    fn(Python<'_>, &Bound<'_, PyAny>) -> Option<ProcessorClassImportPath>;

/// Every media built-in marker the wheel exports, by its import-path resolver.
const NATIVE_BUILTIN_MARKER_IMPORT_PATH_RESOLVERS: [NativeProcessorClassImportPathIfItIs; 13] = [
    PythonTestPatternSourceBlock::native_processor_class_import_path_if_it_is,
    PythonCameraSourceBlock::native_processor_class_import_path_if_it_is,
    PythonDisplayWindowBlock::native_processor_class_import_path_if_it_is,
    PythonMicrophoneSourceBlock::native_processor_class_import_path_if_it_is,
    PythonSpeakerSinkBlock::native_processor_class_import_path_if_it_is,
    PythonH264EncoderBlock::native_processor_class_import_path_if_it_is,
    PythonH264DecoderBlock::native_processor_class_import_path_if_it_is,
    PythonH265EncoderBlock::native_processor_class_import_path_if_it_is,
    PythonH265DecoderBlock::native_processor_class_import_path_if_it_is,
    PythonOpusEncoderBlock::native_processor_class_import_path_if_it_is,
    PythonOpusDecoderBlock::native_processor_class_import_path_if_it_is,
    PythonMp4SinkBlock::native_processor_class_import_path_if_it_is,
    PythonVirtualCameraSinkBlock::native_processor_class_import_path_if_it_is,
];

/// Resolve a Python object to a native built-in's class import path, if it is
/// one of the wheel-exported marker type objects. On a platform where a
/// marker's native processor is not compiled in, the answer is an explicit
/// unsupported-platform error rather than the generic "not a processor"
/// rejection.
pub(crate) fn native_builtin_class_import_path(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<Option<ProcessorClassImportPath>> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    if processor_class.is(python.get_type::<PythonDisplayWindowBlock>()) {
        return Err(PyRuntimeError::new_err(
            "DisplayWindow runs on Linux and macOS; this platform is not supported by the \
             streamlib wheel yet",
        ));
    }
    #[cfg(not(target_os = "linux"))]
    if processor_class.is(python.get_type::<PythonVirtualCameraSinkBlock>()) {
        return Err(PyRuntimeError::new_err(
            "VirtualCameraSink is Linux-only today; this platform is not supported by the \
             streamlib wheel yet",
        ));
    }
    Ok(NATIVE_BUILTIN_MARKER_IMPORT_PATH_RESOLVERS
        .iter()
        .find_map(|import_path_if_it_is| import_path_if_it_is(python, processor_class)))
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
