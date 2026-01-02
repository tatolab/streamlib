// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for StreamLib via PyO3.
//!
//! This crate provides the `PythonHostProcessor` which enables running
//! Python-defined processors within the Rust runtime.

use pyo3::prelude::*;

mod gpu_context_binding;
mod processor_context_proxy;
mod python_host_processor;
mod shader_handle;
mod venv_manager;
mod video_frame_binding;

pub use python_host_processor::{PythonHostProcessor, PythonHostProcessorConfig};

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
    m.add_class::<processor_context_proxy::PyInputPortsProxy>()?;
    m.add_class::<processor_context_proxy::PyOutputPortsProxy>()?;
    m.add_class::<processor_context_proxy::PyInputPort>()?;
    m.add_class::<processor_context_proxy::PyOutputPort>()?;

    Ok(())
}
