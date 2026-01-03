// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for StreamLib via PyO3.
//!
//! This crate provides the `PythonHostProcessor` which enables running
//! Python-defined processors within the Rust runtime.

use pyo3::prelude::*;

mod frame_binding;
mod gpu_context_binding;
mod processor_context_proxy;
mod python_continuous_host_processor;
mod python_host_processor;
mod python_manual_host_processor;
mod python_processor_core;
mod runtime_init;
pub mod schema_binding;
mod schema_field_mappers;
mod shader_handle;
mod venv_manager;
mod video_frame_binding;
mod wheel_cache;

// Re-export all Python host processor variants
pub use python_continuous_host_processor::PythonContinuousHostProcessor;
pub use python_host_processor::{
    PythonHostProcessor, PythonHostProcessorConfig, PythonReactiveHostProcessor,
};
pub use python_manual_host_processor::PythonManualHostProcessor;
pub use python_processor_core::PythonProcessorConfig;

/// StreamLib Python native bindings module.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize pyo3-log to bridge Python logging to Rust tracing
    pyo3_log::init();

    // Register types
    m.add_class::<video_frame_binding::PyVideoFrame>()?;
    m.add_class::<gpu_context_binding::PyGpuContext>()?;
    m.add_class::<shader_handle::PyGpuTexture>()?;
    m.add_class::<shader_handle::PyCompiledShader>()?;
    m.add_class::<processor_context_proxy::PyProcessorContext>()?;
    m.add_class::<processor_context_proxy::PyInputPortProxy>()?;
    m.add_class::<processor_context_proxy::PyOutputPortProxy>()?;
    m.add_class::<frame_binding::PyFrame>()?;

    // Schema API
    m.add_class::<schema_binding::PySchema>()?;
    m.add_function(wrap_pyfunction!(schema_binding::create_schema, m)?)?;
    m.add_function(wrap_pyfunction!(schema_binding::schema_exists, m)?)?;
    m.add_function(wrap_pyfunction!(schema_binding::list_schemas, m)?)?;

    Ok(())
}
