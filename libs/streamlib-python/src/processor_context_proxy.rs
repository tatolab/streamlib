// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for ProcessorContext and input/output port proxies.

use parking_lot::RwLock;
use pyo3::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

use crate::gpu_context_binding::PyGpuContext;
use crate::video_frame_binding::PyVideoFrame;

/// Proxy for reading from input ports.
///
/// Access via `ctx.inputs.{port_name}.read()`.
#[pyclass(name = "InputPortsProxy")]
#[derive(Clone)]
pub struct PyInputPortsProxy {
    /// Map of port_name -> latest frame (if any)
    frames: Arc<RwLock<HashMap<String, Option<PyVideoFrame>>>>,
}

impl PyInputPortsProxy {
    pub fn new() -> Self {
        Self {
            frames: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set the frame for a port (called from Rust before Python process()).
    pub fn set_frame(&self, port_name: &str, frame: Option<PyVideoFrame>) {
        self.frames.write().insert(port_name.to_string(), frame);
    }
}

impl Default for PyInputPortsProxy {
    fn default() -> Self {
        Self::new()
    }
}

#[pymethods]
impl PyInputPortsProxy {
    /// Access input port by name.
    ///
    /// Returns an InputPort object with a `read()` method.
    fn __getattr__(&self, name: &str) -> PyResult<PyInputPort> {
        Ok(PyInputPort {
            port_name: name.to_string(),
            frames: Arc::clone(&self.frames),
        })
    }
}

/// Single input port accessor.
///
/// Created via `ctx.inputs.{port_name}`.
#[pyclass(name = "InputPort")]
#[derive(Clone)]
pub struct PyInputPort {
    port_name: String,
    frames: Arc<RwLock<HashMap<String, Option<PyVideoFrame>>>>,
}

#[pymethods]
impl PyInputPort {
    /// Read the latest frame from this input.
    ///
    /// Returns VideoFrame or None if no data available.
    fn read(&self) -> Option<PyVideoFrame> {
        self.frames.read().get(&self.port_name).cloned().flatten()
    }

    fn __repr__(&self) -> String {
        format!("InputPort('{}')", self.port_name)
    }
}

/// Proxy for writing to output ports.
///
/// Access via `ctx.outputs.{port_name}.write(frame)`.
#[pyclass(name = "OutputPortsProxy")]
#[derive(Clone)]
pub struct PyOutputPortsProxy {
    /// Map of port_name -> pending frame to write
    frames: Arc<RwLock<HashMap<String, Option<PyVideoFrame>>>>,
}

impl PyOutputPortsProxy {
    pub fn new() -> Self {
        Self {
            frames: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Take the pending frame for a port (called from Rust after Python process()).
    pub fn take_frame(&self, port_name: &str) -> Option<PyVideoFrame> {
        self.frames.write().remove(port_name).flatten()
    }

    /// Get all port names that have pending frames.
    pub fn port_names_with_frames(&self) -> Vec<String> {
        self.frames
            .read()
            .iter()
            .filter_map(|(k, v)| if v.is_some() { Some(k.clone()) } else { None })
            .collect()
    }
}

impl Default for PyOutputPortsProxy {
    fn default() -> Self {
        Self::new()
    }
}

#[pymethods]
impl PyOutputPortsProxy {
    /// Access output port by name.
    ///
    /// Returns an OutputPort object with a `write()` method.
    fn __getattr__(&self, name: &str) -> PyResult<PyOutputPort> {
        Ok(PyOutputPort {
            port_name: name.to_string(),
            frames: Arc::clone(&self.frames),
        })
    }
}

/// Single output port accessor.
///
/// Created via `ctx.outputs.{port_name}`.
#[pyclass(name = "OutputPort")]
#[derive(Clone)]
pub struct PyOutputPort {
    port_name: String,
    frames: Arc<RwLock<HashMap<String, Option<PyVideoFrame>>>>,
}

#[pymethods]
impl PyOutputPort {
    /// Write a frame to this output.
    ///
    /// The frame will be sent to downstream processors.
    fn write(&self, frame: PyVideoFrame) {
        self.frames
            .write()
            .insert(self.port_name.clone(), Some(frame));
    }

    fn __repr__(&self) -> String {
        format!("OutputPort('{}')", self.port_name)
    }
}

/// Processor execution context passed to Python lifecycle methods.
///
/// Provides access to input/output ports and GPU context.
#[pyclass(name = "ProcessorContext")]
pub struct PyProcessorContext {
    #[pyo3(get)]
    inputs: Py<PyInputPortsProxy>,

    #[pyo3(get)]
    outputs: Py<PyOutputPortsProxy>,

    #[pyo3(get)]
    gpu: Py<PyGpuContext>,
}

impl PyProcessorContext {
    pub fn new(
        py: Python<'_>,
        inputs: PyInputPortsProxy,
        outputs: PyOutputPortsProxy,
        gpu: PyGpuContext,
    ) -> PyResult<Self> {
        Ok(Self {
            inputs: Py::new(py, inputs)?,
            outputs: Py::new(py, outputs)?,
            gpu: Py::new(py, gpu)?,
        })
    }

    /// Get the inputs proxy for setting frames from Rust.
    pub fn inputs_proxy(&self, py: Python<'_>) -> PyInputPortsProxy {
        self.inputs.borrow(py).clone()
    }

    /// Get the outputs proxy for taking frames from Rust.
    pub fn outputs_proxy(&self, py: Python<'_>) -> PyOutputPortsProxy {
        self.outputs.borrow(py).clone()
    }
}

#[pymethods]
impl PyProcessorContext {
    fn __repr__(&self) -> String {
        "ProcessorContext(inputs=..., outputs=..., gpu=...)".to_string()
    }
}
