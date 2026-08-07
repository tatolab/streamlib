// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel-exported names for the native media built-ins.
//!
//! `streamlib.TestPatternSource` is a marker class: never instantiated, never
//! subclassed, carrying no Python behavior. `Runtime.add` recognizes the type
//! object itself and resolves it to the statically-linked native processor —
//! per-frame paths never enter the interpreter.

// Only the unsupported-platform arms below raise, and they compile away on
// Linux — where both built-ins resolve.
#[cfg(not(target_os = "linux"))]
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::processors::ProcessorTypeReference;

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

/// Resolve a Python object to a native built-in's type reference, if it is
/// one of the wheel-exported marker type objects. The identity comes from the
/// native processor's own declaration — authored once, in the built-ins
/// crate. On a platform where a marker's native processor is not compiled
/// in, the answer is an explicit unsupported-platform error rather than the
/// generic "not a processor" rejection.
pub(crate) fn native_builtin_type_reference(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<Option<ProcessorTypeReference>> {
    if processor_class.is(python.get_type::<PythonTestPatternSourceBlock>()) {
        return Ok(Some(
            streamlib_media_builtins::TestPatternSource::Processor::schema_ident().into(),
        ));
    }
    if processor_class.is(python.get_type::<PythonCameraSourceBlock>()) {
        #[cfg(target_os = "linux")]
        return Ok(Some(
            streamlib_media_builtins::CameraSource::Processor::schema_ident().into(),
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
            streamlib_media_builtins::DisplayWindow::Processor::schema_ident().into(),
        ));
        #[cfg(not(target_os = "linux"))]
        return Err(PyRuntimeError::new_err(
            "DisplayWindow is Linux-only today; this platform is not supported by the \
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
