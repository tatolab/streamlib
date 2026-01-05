// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for ProcessorContext and input/output port proxies.
//!
//! New API:
//!   ctx.input("video_in").get()        # Get whole frame as dict
//!   ctx.input("video_in").get("width") # Get specific field
//!   ctx.output("video_out").set({...}) # Write frame from dict

use parking_lot::RwLock;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::HashMap;
use std::sync::Arc;
use streamlib::GpuContext;

use crate::frame_binding::{video_frame_from_dict, PyFrame};
use crate::gpu_context_binding::PyGpuContext;

/// Port metadata from Python decorators.
#[derive(Clone, Debug)]
pub struct PortMetadata {
    pub name: String,
    pub schema: Option<String>,
    pub description: String,
}

/// Storage for input frames (set from Rust, read from Python).
#[derive(Clone)]
pub struct InputFrameStorage {
    frames: Arc<RwLock<HashMap<String, Option<PyFrame>>>>,
    metadata: Arc<RwLock<HashMap<String, PortMetadata>>>,
}

impl InputFrameStorage {
    pub fn new() -> Self {
        Self {
            frames: Arc::new(RwLock::new(HashMap::new())),
            metadata: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn register_port(&self, meta: PortMetadata) {
        self.metadata.write().insert(meta.name.clone(), meta);
    }

    pub fn set_frame(&self, port_name: &str, frame: Option<PyFrame>) {
        self.frames.write().insert(port_name.to_string(), frame);
    }

    pub fn get_frame(&self, port_name: &str) -> Option<PyFrame> {
        self.frames.read().get(port_name).cloned().flatten()
    }

    pub fn get_schema(&self, port_name: &str) -> Option<String> {
        self.metadata
            .read()
            .get(port_name)
            .and_then(|m| m.schema.clone())
    }
}

impl Default for InputFrameStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// Storage for output frames (set from Python, read from Rust).
#[derive(Clone)]
pub struct OutputFrameStorage {
    frames: Arc<RwLock<HashMap<String, Option<PyFrame>>>>,
    metadata: Arc<RwLock<HashMap<String, PortMetadata>>>,
    gpu_context: Arc<GpuContext>,
}

impl OutputFrameStorage {
    pub fn new(gpu_context: Arc<GpuContext>) -> Self {
        Self {
            frames: Arc::new(RwLock::new(HashMap::new())),
            metadata: Arc::new(RwLock::new(HashMap::new())),
            gpu_context,
        }
    }

    pub fn register_port(&self, meta: PortMetadata) {
        self.metadata.write().insert(meta.name.clone(), meta);
    }

    pub fn take_frame(&self, port_name: &str) -> Option<PyFrame> {
        self.frames.write().remove(port_name).flatten()
    }

    pub fn set_frame(&self, port_name: &str, frame: PyFrame) {
        self.frames
            .write()
            .insert(port_name.to_string(), Some(frame));
    }

    pub fn get_schema(&self, port_name: &str) -> Option<String> {
        self.metadata
            .read()
            .get(port_name)
            .and_then(|m| m.schema.clone())
    }

    pub fn gpu_context(&self) -> &GpuContext {
        &self.gpu_context
    }

    pub fn port_names_with_frames(&self) -> Vec<String> {
        self.frames
            .read()
            .iter()
            .filter_map(|(k, v)| if v.is_some() { Some(k.clone()) } else { None })
            .collect()
    }
}

/// Input port proxy returned by ctx.input("port_name").
///
/// Provides get() method for reading frame data.
#[pyclass(name = "InputPortProxy")]
#[derive(Clone)]
pub struct PyInputPortProxy {
    port_name: String,
    storage: InputFrameStorage,
}

#[pymethods]
impl PyInputPortProxy {
    /// Get the whole frame as a dict, or a specific field by name.
    ///
    /// Usage:
    ///   ctx.input("video_in").get()          # Returns dict with all fields
    ///   ctx.input("video_in").get("width")   # Returns just the width field
    ///
    /// Returns None if no frame is available.
    #[pyo3(signature = (field=None))]
    fn get(&self, py: Python<'_>, field: Option<&str>) -> PyResult<Option<Py<PyAny>>> {
        // Check if schema was set
        let schema = self.storage.get_schema(&self.port_name);
        if schema.is_none() {
            tracing::error!(
                "Input port '{}' has no schema set. Use @input(schema=\"SchemaName\") decorator.",
                self.port_name
            );
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "Input port '{}' has no schema set",
                self.port_name
            )));
        }

        match self.storage.get_frame(&self.port_name) {
            Some(frame) => {
                let result = frame.get_field(py, field)?;
                Ok(Some(result))
            }
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        format!("InputPortProxy('{}')", self.port_name)
    }
}

/// Output port proxy returned by ctx.output("port_name").
///
/// Provides set() method for writing frame data.
#[pyclass(name = "OutputPortProxy")]
#[derive(Clone)]
pub struct PyOutputPortProxy {
    port_name: String,
    storage: OutputFrameStorage,
}

#[pymethods]
impl PyOutputPortProxy {
    /// Write a frame to this output.
    ///
    /// Accepts either a Frame object or a dict with frame fields.
    ///
    /// Usage:
    ///   ctx.output("video_out").set(frame)           # Pass Frame object
    ///   ctx.output("video_out").set({"texture": t, "width": w, "height": h})
    fn set(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        // Check if schema was set
        let schema = self.storage.get_schema(&self.port_name);
        if schema.is_none() {
            tracing::error!(
                "Output port '{}' has no schema set. Use @output(schema=\"SchemaName\") decorator.",
                self.port_name
            );
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "Output port '{}' has no schema set",
                self.port_name
            )));
        }

        let schema_name = schema.unwrap();

        // Check if it's already a PyFrame
        if let Ok(frame) = value.extract::<PyFrame>() {
            self.storage.set_frame(&self.port_name, frame);
            return Ok(());
        }

        // Try to extract as dict and build frame
        if let Ok(dict) = value.cast::<PyDict>() {
            match schema_name.as_str() {
                "VideoFrame" => {
                    let video_frame = video_frame_from_dict(py, dict, self.storage.gpu_context())?;
                    let frame = PyFrame::from_video_frame(video_frame);
                    self.storage.set_frame(&self.port_name, frame);
                    Ok(())
                }
                "AudioFrame" => {
                    // TODO: Implement audio_frame_from_dict
                    Err(pyo3::exceptions::PyNotImplementedError::new_err(
                        "AudioFrame from dict not yet implemented",
                    ))
                }
                "DataFrame" => {
                    // TODO: Implement data_frame_from_dict
                    Err(pyo3::exceptions::PyNotImplementedError::new_err(
                        "DataFrame from dict not yet implemented",
                    ))
                }
                _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Unknown schema: {}",
                    schema_name
                ))),
            }
        } else {
            Err(pyo3::exceptions::PyTypeError::new_err(
                "set() expects a Frame or dict",
            ))
        }
    }

    fn __repr__(&self) -> String {
        format!("OutputPortProxy('{}')", self.port_name)
    }
}

/// Processor execution context passed to Python lifecycle methods.
///
/// Provides access to input/output ports and GPU context.
#[pyclass(name = "ProcessorContext")]
pub struct PyProcessorContext {
    input_storage: InputFrameStorage,
    output_storage: OutputFrameStorage,

    #[pyo3(get)]
    gpu: Py<PyGpuContext>,
}

impl PyProcessorContext {
    pub fn new(py: Python<'_>, gpu_context: Arc<GpuContext>) -> PyResult<Self> {
        let py_gpu = PyGpuContext::new((*gpu_context).clone());
        Ok(Self {
            input_storage: InputFrameStorage::new(),
            output_storage: OutputFrameStorage::new(gpu_context),
            gpu: Py::new(py, py_gpu)?,
        })
    }

    /// Register an input port with metadata.
    pub fn register_input_port(&self, meta: PortMetadata) {
        self.input_storage.register_port(meta);
    }

    /// Register an output port with metadata.
    pub fn register_output_port(&self, meta: PortMetadata) {
        self.output_storage.register_port(meta);
    }

    /// Set the input frame for a port (called from Rust before Python process()).
    pub fn set_input_frame(&self, port_name: &str, frame: Option<PyFrame>) {
        self.input_storage.set_frame(port_name, frame);
    }

    /// Take the output frame for a port (called from Rust after Python process()).
    pub fn take_output_frame(&self, port_name: &str) -> Option<PyFrame> {
        self.output_storage.take_frame(port_name)
    }

    /// Get all port names that have pending output frames.
    pub fn output_port_names_with_frames(&self) -> Vec<String> {
        self.output_storage.port_names_with_frames()
    }
}

#[pymethods]
impl PyProcessorContext {
    /// Current platform name.
    ///
    /// Returns "macos", "linux", or "windows".
    /// Use this to branch on platform-specific behavior.
    #[getter]
    fn platform(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        {
            "macos"
        }
        #[cfg(target_os = "linux")]
        {
            "linux"
        }
        #[cfg(target_os = "windows")]
        {
            "windows"
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            "unknown"
        }
    }

    /// Get an input port proxy by name.
    ///
    /// Usage:
    ///   ctx.input("video_in").get()
    fn input(&self, name: &str) -> PyInputPortProxy {
        PyInputPortProxy {
            port_name: name.to_string(),
            storage: self.input_storage.clone(),
        }
    }

    /// Get an output port proxy by name.
    ///
    /// Usage:
    ///   ctx.output("video_out").set({...})
    fn output(&self, name: &str) -> PyOutputPortProxy {
        PyOutputPortProxy {
            port_name: name.to_string(),
            storage: self.output_storage.clone(),
        }
    }

    fn __repr__(&self) -> String {
        "ProcessorContext(input=..., output=..., gpu=...)".to_string()
    }
}
