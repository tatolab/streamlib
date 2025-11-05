
use pyo3::prelude::*;
use crate::core::VideoFrame;
use std::collections::HashMap;

#[pyclass(name = "VideoFrame")]
#[derive(Clone)]
pub struct PyVideoFrame {
    pub(crate) inner: VideoFrame,
}

#[pymethods]
impl PyVideoFrame {
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }

    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }

    #[getter]
    fn format(&self) -> String {
        format!("{:?}", self.inner.format)
    }

    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp
    }

    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    #[getter]
    fn data(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuTexture;
        let texture_wrapper = PyWgpuTexture {
            texture: self.inner.texture.clone()
        };
        Ok(Py::new(py, texture_wrapper)?.into_py(py))
    }

    fn clone_with_texture(&self, py: Python<'_>, new_texture: &Bound<'_, PyAny>) -> PyResult<PyVideoFrame> {
        use super::gpu_wrappers::PyWgpuTexture;

        let texture_wrapper: Py<PyWgpuTexture> = new_texture.extract()?;
        let texture_arc = texture_wrapper.borrow(py).texture.clone();

        let new_frame = VideoFrame {
            texture: texture_arc,
            format: self.inner.format,
            width: self.inner.width,
            height: self.inner.height,
            timestamp: self.inner.timestamp,
            frame_number: self.inner.frame_number,
            metadata: self.inner.metadata.clone(),
        };

        Ok(PyVideoFrame { inner: new_frame })
    }

    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        self.inner.metadata.as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn __repr__(&self) -> String {
        format!(
            "VideoFrame({}x{}, {:?}, frame={})",
            self.inner.width, self.inner.height, self.inner.format, self.inner.frame_number
        )
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }
}

impl PyVideoFrame {
    pub fn from_rust(frame: VideoFrame) -> Self {
        Self { inner: frame }
    }

    pub fn as_rust(&self) -> &VideoFrame {
        &self.inner
    }

    pub fn into_rust(self) -> VideoFrame {
        self.inner
    }
}
