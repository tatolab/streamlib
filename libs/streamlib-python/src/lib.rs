// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for StreamLib.
//!
//! This crate provides Python processor types that run Python code in isolated
//! subprocesses, enabling dependency isolation, crash safety, and true parallelism.

use pyo3::prelude::*;

mod frame_binding;
mod gl_context_binding;
mod gpu_context_binding;
mod pixel_buffer_binding;
mod processor_context_proxy;
mod python_continuous_host_processor;
mod python_continuous_processor;
mod python_core;
mod python_host_processor;
mod python_manual_host_processor;
mod python_manual_processor;
mod python_processor_core;
mod python_reactive_processor;
mod runtime_init;
pub mod schema_binding;
mod schema_field_mappers;
mod shader_handle;
mod time_context_binding;
mod venv_manager;
mod video_frame_binding;
mod wheel_cache;
mod xpc_channel_binding;

// Re-export Python processor variants (subprocess-based)
pub use python_continuous_processor::PythonContinuousProcessor;
pub use python_manual_processor::PythonManualProcessor;
pub use python_processor_core::PythonProcessorConfig;
pub use python_reactive_processor::PythonReactiveProcessor;

// Deprecated: Re-export old embedded mode processors for backward compatibility
#[deprecated(note = "Use PythonContinuousProcessor instead (subprocess-based)")]
pub use python_continuous_host_processor::PythonContinuousHostProcessor;
#[deprecated(note = "Use PythonReactiveProcessor instead (subprocess-based)")]
pub use python_host_processor::{
    PythonHostProcessor, PythonHostProcessorConfig, PythonReactiveHostProcessor,
};
#[deprecated(note = "Use PythonManualProcessor instead (subprocess-based)")]
pub use python_manual_host_processor::PythonManualHostProcessor;

/// StreamLib Python native bindings module.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize pyo3-log to bridge Python logging to Rust tracing
    pyo3_log::init();

    // Register types
    m.add_class::<video_frame_binding::PyVideoFrame>()?;
    m.add_class::<gpu_context_binding::PyGpuContext>()?;
    m.add_class::<gl_context_binding::PyGlContext>()?;
    m.add_class::<gl_context_binding::PyGlTextureBinding>()?;
    m.add_class::<shader_handle::PyGpuTexture>()?;
    m.add_class::<shader_handle::PyPooledTextureHandle>()?;
    m.add_class::<time_context_binding::PyTimeContext>()?;
    m.add_class::<processor_context_proxy::PyProcessorContext>()?;
    m.add_class::<processor_context_proxy::PyInputPortProxy>()?;
    m.add_class::<processor_context_proxy::PyOutputPortProxy>()?;
    m.add_class::<frame_binding::PyFrame>()?;
    m.add_class::<pixel_buffer_binding::PyPixelFormat>()?;
    m.add_class::<pixel_buffer_binding::PyRhiPixelBuffer>()?;

    // Schema API
    m.add_class::<schema_binding::PySchema>()?;
    m.add_function(wrap_pyfunction!(schema_binding::create_schema, m)?)?;
    m.add_function(wrap_pyfunction!(schema_binding::schema_exists, m)?)?;
    m.add_function(wrap_pyfunction!(schema_binding::list_schemas, m)?)?;

    // XPC frame channel (macOS GPU sharing)
    m.add_class::<xpc_channel_binding::PyXpcFrameChannel>()?;

    Ok(())
}
