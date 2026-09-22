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
use streamlib::sdk::descriptors::ProcessorClassImportPath;

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
}

/// `streamlib.CameraSource` — live camera capture through the engine's video
/// device seam (V4L2 on Linux; a platform with no capture backend refuses at
/// `setup()`).
#[pyclass(name = "CameraSource", module = "streamlib", frozen)]
pub(crate) struct PythonCameraSourceBlock;

/// `streamlib.DisplayWindow` — video frames in a vsync'd window.
#[pyclass(name = "DisplayWindow", module = "streamlib", frozen)]
pub(crate) struct PythonDisplayWindowBlock;

/// `streamlib.MicrophoneSource` — audio capture on whichever backend the
/// chain probed, silence where none exists.
#[pyclass(name = "MicrophoneSource", module = "streamlib", frozen)]
pub(crate) struct PythonMicrophoneSourceBlock;

/// `streamlib.SpeakerSink` — audio playback on whichever backend the chain
/// probed, discarding where none exists.
#[pyclass(name = "SpeakerSink", module = "streamlib", frozen)]
pub(crate) struct PythonSpeakerSinkBlock;

/// `streamlib.H264Encoder` — video frames to H.264 encoded-frame bags via
/// Vulkan Video hardware encode (Linux).
#[pyclass(name = "H264Encoder", module = "streamlib", frozen)]
pub(crate) struct PythonH264EncoderBlock;

/// `streamlib.H264Decoder` — H.264 encoded-frame bags to decoded video
/// frames via Vulkan Video hardware decode (Linux).
#[pyclass(name = "H264Decoder", module = "streamlib", frozen)]
pub(crate) struct PythonH264DecoderBlock;

/// `streamlib.H265Encoder` — video frames to H.265 encoded-frame bags via
/// Vulkan Video hardware encode (Linux).
#[pyclass(name = "H265Encoder", module = "streamlib", frozen)]
pub(crate) struct PythonH265EncoderBlock;

/// `streamlib.H265Decoder` — H.265 encoded-frame bags to decoded video
/// frames via Vulkan Video hardware decode (Linux).
#[pyclass(name = "H265Decoder", module = "streamlib", frozen)]
pub(crate) struct PythonH265DecoderBlock;

/// `streamlib.OpusEncoder` — 20 ms windows of audio to Opus
/// encoded-audio-packet bags via statically linked libopus, on every
/// platform the wheel builds for.
#[pyclass(name = "OpusEncoder", module = "streamlib", frozen)]
pub(crate) struct PythonOpusEncoderBlock;

/// `streamlib.OpusDecoder` — Opus encoded-audio-packet bags to decoded
/// audio blocks via statically linked libopus, on every platform the wheel
/// builds for.
#[pyclass(name = "OpusDecoder", module = "streamlib", frozen)]
pub(crate) struct PythonOpusDecoderBlock;

/// `streamlib.Mp4Sink` — encoded video and audio bags recorded to one
/// fragmented MP4 file, one track per inbound link, on every platform the
/// wheel builds for.
#[pyclass(name = "Mp4Sink", module = "streamlib", frozen)]
pub(crate) struct PythonMp4SinkBlock;

/// `streamlib.VirtualCameraSink` — video frames presented as a virtual
/// camera any Linux application can select (Linux).
#[pyclass(name = "VirtualCameraSink", module = "streamlib", frozen)]
pub(crate) struct PythonVirtualCameraSinkBlock;

/// Resolve a Python object to a native built-in's class import path, if it is
/// one of the wheel-exported marker type objects. The identity comes from the
/// native processor's own declaration — authored once, in the built-ins
/// crate. On a platform where a marker's native processor is not compiled
/// in, the answer is an explicit unsupported-platform error rather than the
/// generic "not a processor" rejection.
pub(crate) fn native_builtin_class_import_path(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<Option<ProcessorClassImportPath>> {
    if processor_class.is(python.get_type::<PythonTestPatternSourceBlock>()) {
        return Ok(Some(
            streamlib_media_builtins::TestPatternSource::Processor::processor_class_import_path(),
        ));
    }
    if processor_class.is(python.get_type::<PythonCameraSourceBlock>()) {
        return Ok(Some(
            streamlib_media_builtins::CameraSource::Processor::processor_class_import_path(),
        ));
    }
    if processor_class.is(python.get_type::<PythonDisplayWindowBlock>()) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return Ok(Some(
            streamlib_media_builtins::DisplayWindow::Processor::processor_class_import_path(),
        ));
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(PyRuntimeError::new_err(
            "DisplayWindow runs on Linux and macOS; this platform is not supported by the \
             streamlib wheel yet",
        ));
    }
    if processor_class.is(python.get_type::<PythonMicrophoneSourceBlock>()) {
        return Ok(Some(
            streamlib_media_builtins::MicrophoneSource::Processor::processor_class_import_path(),
        ));
    }
    if processor_class.is(python.get_type::<PythonSpeakerSinkBlock>()) {
        return Ok(Some(
            streamlib_media_builtins::SpeakerSink::Processor::processor_class_import_path(),
        ));
    }
    if processor_class.is(python.get_type::<PythonH264EncoderBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::H264Encoder::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "H264Encoder is Linux-only (Vulkan Video hardware encode); this platform is \
             not supported by the streamlib wheel yet",
        ));
    }
    if processor_class.is(python.get_type::<PythonH264DecoderBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::H264Decoder::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "H264Decoder is Linux-only (Vulkan Video hardware decode); this platform is \
             not supported by the streamlib wheel yet",
        ));
    }
    if processor_class.is(python.get_type::<PythonH265EncoderBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::H265Encoder::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "H265Encoder is Linux-only (Vulkan Video hardware encode); this platform is \
             not supported by the streamlib wheel yet",
        ));
    }
    if processor_class.is(python.get_type::<PythonH265DecoderBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::H265Decoder::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "H265Decoder is Linux-only (Vulkan Video hardware decode); this platform is \
             not supported by the streamlib wheel yet",
        ));
    }
    if processor_class.is(python.get_type::<PythonOpusEncoderBlock>()) {
        return Ok(Some(
            streamlib_media_builtins::OpusEncoder::Processor::processor_class_import_path(),
        ));
    }
    if processor_class.is(python.get_type::<PythonOpusDecoderBlock>()) {
        return Ok(Some(
            streamlib_media_builtins::OpusDecoder::Processor::processor_class_import_path(),
        ));
    }
    if processor_class.is(python.get_type::<PythonMp4SinkBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::Mp4Sink::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "Mp4Sink is Linux-only today: its muxer reads parameter sets through the Vulkan \
             Video NAL parser; this platform is not supported by the streamlib wheel yet",
        ));
    }
    if processor_class.is(python.get_type::<PythonVirtualCameraSinkBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::VirtualCameraSink::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "VirtualCameraSink is Linux-only today; this platform is not supported by the \
             streamlib wheel yet",
        ));
    }
    Ok(None)
}

/// Register the native built-in processor types on the process-wide registry.
/// Idempotent; called once at module import.
pub(crate) fn register_native_builtin_processor_types() {
    streamlib_media_builtins::register_media_builtin_processor_types();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
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
}
