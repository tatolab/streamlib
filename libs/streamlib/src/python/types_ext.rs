use super::PyVideoFrame;
use crate::core::{AudioFrame, DataFrame, GpuContext, StreamInput, StreamOutput, VideoFrame};
use parking_lot::Mutex;
use pyo3::prelude::*;
use std::sync::Arc;

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

// Audio frame wrappers for different channel counts

#[pyclass(name = "StreamInputAudio1", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInputAudio1 {
    pub(crate) port: Arc<Mutex<StreamInput<AudioFrame<1>>>>,
}

impl PyStreamInputAudio1 {
    pub fn from_port(port: StreamInput<AudioFrame<1>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamOutputAudio1", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutputAudio1 {
    pub(crate) port: Arc<Mutex<StreamOutput<AudioFrame<1>>>>,
}

impl PyStreamOutputAudio1 {
    pub fn from_port(port: StreamOutput<AudioFrame<1>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamInputAudio2", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInputAudio2 {
    pub(crate) port: Arc<Mutex<StreamInput<AudioFrame<2>>>>,
}

impl PyStreamInputAudio2 {
    pub fn from_port(port: StreamInput<AudioFrame<2>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamOutputAudio2", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutputAudio2 {
    pub(crate) port: Arc<Mutex<StreamOutput<AudioFrame<2>>>>,
}

impl PyStreamOutputAudio2 {
    pub fn from_port(port: StreamOutput<AudioFrame<2>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamInputAudio4", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInputAudio4 {
    pub(crate) port: Arc<Mutex<StreamInput<AudioFrame<4>>>>,
}

impl PyStreamInputAudio4 {
    pub fn from_port(port: StreamInput<AudioFrame<4>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamOutputAudio4", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutputAudio4 {
    pub(crate) port: Arc<Mutex<StreamOutput<AudioFrame<4>>>>,
}

impl PyStreamOutputAudio4 {
    pub fn from_port(port: StreamOutput<AudioFrame<4>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamInputAudio6", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInputAudio6 {
    pub(crate) port: Arc<Mutex<StreamInput<AudioFrame<6>>>>,
}

impl PyStreamInputAudio6 {
    pub fn from_port(port: StreamInput<AudioFrame<6>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamOutputAudio6", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutputAudio6 {
    pub(crate) port: Arc<Mutex<StreamOutput<AudioFrame<6>>>>,
}

impl PyStreamOutputAudio6 {
    pub fn from_port(port: StreamOutput<AudioFrame<6>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamInputAudio8", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInputAudio8 {
    pub(crate) port: Arc<Mutex<StreamInput<AudioFrame<8>>>>,
}

impl PyStreamInputAudio8 {
    pub fn from_port(port: StreamInput<AudioFrame<8>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamOutputAudio8", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutputAudio8 {
    pub(crate) port: Arc<Mutex<StreamOutput<AudioFrame<8>>>>,
}

impl PyStreamOutputAudio8 {
    pub fn from_port(port: StreamOutput<AudioFrame<8>>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

// Data frame wrappers

#[pyclass(name = "StreamInputData", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamInputData {
    pub(crate) port: Arc<Mutex<StreamInput<DataFrame>>>,
}

impl PyStreamInputData {
    pub fn from_port(port: StreamInput<DataFrame>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
    }
}

#[pyclass(name = "StreamOutputData", module = "streamlib")]
#[derive(Clone)]
pub struct PyStreamOutputData {
    pub(crate) port: Arc<Mutex<StreamOutput<DataFrame>>>,
}

impl PyStreamOutputData {
    pub fn from_port(port: StreamOutput<DataFrame>) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
        }
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

        Ok(Py::new(
            py,
            PyWgpuTexture {
                texture: std::sync::Arc::new(texture),
            },
        )?
        .into_py(py))
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

#[pyclass(name = "RuntimeContext", module = "streamlib")]
pub struct PyRuntimeContext {
    pub(crate) inner: crate::core::RuntimeContext,
}

#[pymethods]
impl PyRuntimeContext {
    #[getter]
    fn gpu(&self) -> PyGpuContext {
        PyGpuContext::from_rust(&self.inner.gpu)
    }

    fn __repr__(&self) -> String {
        "RuntimeContext(gpu, audio)".to_string()
    }
}

impl PyRuntimeContext {
    pub fn from_rust(context: &crate::core::RuntimeContext) -> Self {
        Self {
            inner: context.clone(),
        }
    }
}
