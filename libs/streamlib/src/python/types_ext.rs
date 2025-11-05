
use pyo3::prelude::*;
use pyo3::exceptions::PyAttributeError;
use crate::core::{StreamInput, StreamOutput, VideoFrame, TimedTick, GpuContext};
use super::PyVideoFrame;
use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::HashMap;

#[pyclass(name = "StreamInput", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInput {
    pub(crate) port: Arc<Mutex<StreamInput<VideoFrame>>>,
}

#[pymethods]
impl PyStreamInput {
    #[new]
    #[pyo3(signature = (_type_hint=None))]
    fn new(_type_hint: Option<PyObject>) -> Self {
        use crate::core::StreamInput;
        Self {
            port: Arc::new(Mutex::new(StreamInput::new("placeholder"))),
        }
    }

    fn read_latest(&self) -> Option<PyVideoFrame> {
        let port = self.port.lock();
        port.read_latest().map(PyVideoFrame::from_rust)
    }

    fn has_data(&self) -> bool {
        let port = self.port.lock();
        port.read_latest().is_some()
    }

    fn __repr__(&self) -> String {
        "StreamInput(VideoFrame)".to_string()
    }
}

impl PyStreamInput {
    pub fn from_port(port: StreamInput<VideoFrame>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamOutput", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutput {
    pub(crate) port: Arc<Mutex<StreamOutput<VideoFrame>>>,
}

#[pymethods]
impl PyStreamOutput {
    #[new]
    #[pyo3(signature = (_type_hint=None))]
    fn new(_type_hint: Option<PyObject>) -> Self {
        use crate::core::StreamOutput;
        Self {
            port: Arc::new(Mutex::new(StreamOutput::new("placeholder"))),
        }
    }

    fn write(&self, frame: PyVideoFrame) {
        let port = self.port.lock();
        port.write(frame.into_rust());
    }

    fn __repr__(&self) -> String {
        "StreamOutput(VideoFrame)".to_string()
    }
}

impl PyStreamOutput {
    pub fn from_port(port: StreamOutput<VideoFrame>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "TimedTick", module = "streamlib")]
#[derive(Clone)]
pub struct PyTimedTick {
    pub(crate) inner: TimedTick,
}

#[pymethods]
impl PyTimedTick {
    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp
    }

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
    pub fn from_rust(tick: TimedTick) -> Self {
        Self { inner: tick }
    }
}

#[pyclass(name = "GpuContext", module = "streamlib")]
pub struct PyGpuContext {
    pub(crate) inner: GpuContext,
}

#[pymethods]
impl PyGpuContext {
    #[getter]
    fn device(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuDevice;
        let device = PyWgpuDevice::new(self.inner.clone());
        Ok(Py::new(py, device)?.into_py(py))
    }

    #[getter]
    fn queue(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuQueue;
        let queue = PyWgpuQueue::new(self.inner.clone());
        Ok(Py::new(py, queue)?.into_py(py))
    }

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
    pub fn from_rust(context: &GpuContext) -> Self {
        Self {
            inner: context.clone(),
        }
    }
}

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
    pub fn new(ports: HashMap<String, PyStreamInput>) -> Self {
        Self { ports }
    }
}

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
    pub fn new(ports: HashMap<String, PyStreamOutput>) -> Self {
        Self { ports }
    }
}
