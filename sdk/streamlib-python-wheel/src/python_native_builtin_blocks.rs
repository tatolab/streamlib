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

/// `streamlib.CameraSource` — live V4L2 camera capture (Linux).
#[pyclass(name = "CameraSource", module = "streamlib", frozen)]
pub(crate) struct PythonCameraSourceBlock;

/// `streamlib.DisplayWindow` — video frames in a vsync'd window (Linux).
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
/// encoded-audio-packet bags via libopus.
#[pyclass(name = "OpusEncoder", module = "streamlib", frozen)]
pub(crate) struct PythonOpusEncoderBlock;

/// `streamlib.OpusDecoder` — Opus encoded-audio-packet bags to decoded
/// audio blocks via libopus.
#[pyclass(name = "OpusDecoder", module = "streamlib", frozen)]
pub(crate) struct PythonOpusDecoderBlock;

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
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::CameraSource::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "CameraSource is Linux-only (V4L2 capture); this platform is not supported \
             by the streamlib wheel yet",
        ));
    }
    if processor_class.is(python.get_type::<PythonDisplayWindowBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::DisplayWindow::Processor::processor_class_import_path(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "DisplayWindow is Linux-only today; this platform is not supported by the \
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
    Ok(None)
}

/// Register the native built-in processor types on the process-wide registry.
/// Idempotent; called once at module import.
pub(crate) fn register_native_builtin_processor_types() {
    streamlib_media_builtins::register_media_builtin_processor_types();
}
