//! Extended Python types for ports, timing, and GPU context

use pyo3::prelude::*;
use pyo3::exceptions::PyAttributeError;
use crate::core::{StreamInput, StreamOutput, VideoFrame, TimedTick, GpuContext};
use super::PyVideoFrame;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

/// Python wrapper for StreamInput<VideoFrame>
#[pyclass(name = "StreamInput", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInput {
    /// Reference to the Rust port (wrapped in Arc<Mutex<>> for thread safety)
    pub(crate) port: Arc<Mutex<StreamInput<VideoFrame>>>,
}

#[pymethods]
impl PyStreamInput {
    /// Constructor for use as a type marker in class definitions
    ///
    /// This allows syntax like: `video = StreamInput(VideoFrame)`
    /// The actual port is created by Rust and injected at runtime.
    #[new]
    #[pyo3(signature = (_type_hint=None))]
    fn new(_type_hint: Option<PyObject>) -> Self {
        // This is just a marker for syntax - never actually used
        // The real ports are created in processor.rs and injected in on_start()
        use crate::core::StreamInput;
        Self {
            port: Arc::new(Mutex::new(StreamInput::new("placeholder"))),
        }
    }

    /// Read the latest frame from the input port
    fn read_latest(&self) -> Option<PyVideoFrame> {
        let port = self.port.lock().unwrap();
        port.read_latest().map(PyVideoFrame::from_rust)
    }

    /// Check if port has data available
    fn has_data(&self) -> bool {
        let port = self.port.lock().unwrap();
        port.read_latest().is_some()
    }

    fn __repr__(&self) -> String {
        "StreamInput(VideoFrame)".to_string()
    }
}

impl PyStreamInput {
    /// Create from Rust StreamInput
    pub fn from_port(port: StreamInput<VideoFrame>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

/// Python wrapper for StreamOutput<VideoFrame>
#[pyclass(name = "StreamOutput", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutput {
    /// Reference to the Rust port (wrapped in Arc<Mutex<>> for thread safety)
    pub(crate) port: Arc<Mutex<StreamOutput<VideoFrame>>>,
}

#[pymethods]
impl PyStreamOutput {
    /// Constructor for use as a type marker in class definitions
    ///
    /// This allows syntax like: `video = StreamOutput(VideoFrame)`
    /// The actual port is created by Rust and injected at runtime.
    #[new]
    #[pyo3(signature = (_type_hint=None))]
    fn new(_type_hint: Option<PyObject>) -> Self {
        // This is just a marker for syntax - never actually used
        // The real ports are created in processor.rs and injected in on_start()
        use crate::core::StreamOutput;
        Self {
            port: Arc::new(Mutex::new(StreamOutput::new("placeholder"))),
        }
    }

    /// Write a frame to the output port
    fn write(&self, frame: PyVideoFrame) {
        let port = self.port.lock().unwrap();
        port.write(frame.into_rust());
    }

    fn __repr__(&self) -> String {
        "StreamOutput(VideoFrame)".to_string()
    }
}

impl PyStreamOutput {
    /// Create from Rust StreamOutput
    pub fn from_port(port: StreamOutput<VideoFrame>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

/// Python wrapper for TimedTick
#[pyclass(name = "TimedTick", module = "streamlib")]
#[derive(Clone)]
pub struct PyTimedTick {
    pub(crate) inner: TimedTick,
}

#[pymethods]
impl PyTimedTick {
    /// Get frame number
    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    /// Get timestamp in seconds
    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp
    }

    /// Get delta time since last frame
    #[getter]
    fn delta_time(&self) -> f64 {
        self.inner.delta_time
    }

    fn __repr__(&self) -> String {
        format!(
            "TimedTick(frame={}, time={:.3}s, dt={:.3}s)",
            self.inner.frame_number, self.inner.timestamp, self.inner.delta_time
        )
    }
}

impl PyTimedTick {
    /// Create from Rust TimedTick
    pub fn from_rust(tick: TimedTick) -> Self {
        Self { inner: tick }
    }
}

/// Python wrapper for GpuContext
///
/// Provides device, queue, and helper methods for GPU operations
#[pyclass(name = "GpuContext", module = "streamlib")]
pub struct PyGpuContext {
    pub(crate) inner: GpuContext,
}

#[pymethods]
impl PyGpuContext {
    /// Get wgpu device wrapper
    #[getter]
    fn device(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuDevice;
        let device = PyWgpuDevice::new(self.inner.clone());
        Ok(Py::new(py, device)?.into_py(py))
    }

    /// Get wgpu queue wrapper
    #[getter]
    fn queue(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuQueue;
        let queue = PyWgpuQueue::new(self.inner.clone());
        Ok(Py::new(py, queue)?.into_py(py))
    }

    /// Create a new texture (convenience method matching old API)
    fn create_texture(&self, py: Python<'_>, width: u32, height: u32) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuTexture;

        let device = self.inner.device();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        Ok(Py::new(py, PyWgpuTexture { texture: std::sync::Arc::new(texture) })?.into_py(py))
    }

    fn __repr__(&self) -> String {
        "GpuContext(device, queue)".to_string()
    }
}

impl PyGpuContext {
    /// Create from Rust GpuContext
    pub fn from_rust(context: &GpuContext) -> Self {
        Self {
            inner: context.clone(),
        }
    }
}

/// Python wrapper for a collection of input ports
#[pyclass(name = "InputPorts", module = "streamlib")]
#[derive(Clone)]
pub struct PyInputPorts {
    pub(crate) ports: HashMap<String, PyStreamInput>,
}

#[pymethods]
impl PyInputPorts {
    fn __getattr__(&self, name: String) -> PyResult<PyStreamInput> {
        self.ports.get(&name)
            .cloned()
            .ok_or_else(|| PyAttributeError::new_err(
                format!("Input port '{}' not found", name)
            ))
    }

    fn __repr__(&self) -> String {
        let port_names: Vec<&str> = self.ports.keys().map(|s| s.as_str()).collect();
        format!("InputPorts({})", port_names.join(", "))
    }
}

impl PyInputPorts {
    /// Create new input ports collection
    pub fn new(ports: HashMap<String, PyStreamInput>) -> Self {
        Self { ports }
    }
}

/// Python wrapper for a collection of output ports
#[pyclass(name = "OutputPorts", module = "streamlib")]
#[derive(Clone)]
pub struct PyOutputPorts {
    pub(crate) ports: HashMap<String, PyStreamOutput>,
}

#[pymethods]
impl PyOutputPorts {
    fn __getattr__(&self, name: String) -> PyResult<PyStreamOutput> {
        self.ports.get(&name)
            .cloned()
            .ok_or_else(|| PyAttributeError::new_err(
                format!("Output port '{}' not found", name)
            ))
    }

    fn __repr__(&self) -> String {
        let port_names: Vec<&str> = self.ports.keys().map(|s| s.as_str()).collect();
        format!("OutputPorts({})", port_names.join(", "))
    }
}

impl PyOutputPorts {
    /// Create new output ports collection
    pub fn new(ports: HashMap<String, PyStreamOutput>) -> Self {
        Self { ports }
    }
}
