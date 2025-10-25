//! Python bindings for streamlib core types

use pyo3::prelude::*;
use crate::core::VideoFrame;
use std::collections::HashMap;
use std::sync::Arc;

/// Python wrapper for VideoFrame
///
/// Represents a video frame with GPU texture data.
/// Python code sees this as an opaque handle - the actual GPU memory stays on GPU.
#[pyclass(name = "VideoFrame")]
#[derive(Clone)]
pub struct PyVideoFrame {
    /// The underlying Rust VideoFrame
    pub(crate) inner: VideoFrame,
}

#[pymethods]
impl PyVideoFrame {
    /// Get frame width
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }

    /// Get frame height
    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }

    /// Get frame format as string
    #[getter]
    fn format(&self) -> String {
        format!("{:?}", self.inner.format)
    }

    /// Get frame timestamp
    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp
    }

    /// Get frame number
    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    /// Get the GPU texture (for shader input/output)
    #[getter]
    fn data(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuTexture;
        // Get raw pointer to the Arc's inner texture
        let texture_ptr = self.inner.texture.as_ref() as *const wgpu::Texture as usize;
        let texture_wrapper = PyWgpuTexture { handle: texture_ptr };
        Ok(Py::new(py, texture_wrapper)?.into_py(py))
    }

    /// Clone frame with a new texture (for shader output)
    fn clone_with_texture(&self, py: Python<'_>, new_texture: &Bound<'_, PyAny>) -> PyResult<PyVideoFrame> {
        use super::gpu_wrappers::PyWgpuTexture;

        // Extract the texture wrapper
        let texture_wrapper: Py<PyWgpuTexture> = new_texture.extract()?;
        let texture_handle = texture_wrapper.borrow(py).handle;

        // SAFETY: texture_handle must be a valid wgpu::Texture pointer
        unsafe {
            let texture_ref = &*(texture_handle as *const wgpu::Texture);
            // Clone the texture Arc to share ownership
            let texture_arc = Arc::new(texture_ref.clone());

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
    }

    /// Get metadata as dictionary
    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        // Convert Option<HashMap<String, MetadataValue>> to HashMap<String, String>
        self.inner.metadata.as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// String representation
    fn __repr__(&self) -> String {
        format!(
            "VideoFrame({}x{}, {:?}, frame={})",
            self.inner.width, self.inner.height, self.inner.format, self.inner.frame_number
        )
    }

    /// String representation
    fn __str__(&self) -> String {
        self.__repr__()
    }
}

impl PyVideoFrame {
    /// Create from Rust VideoFrame
    pub fn from_rust(frame: VideoFrame) -> Self {
        Self { inner: frame }
    }

    /// Get reference to inner Rust VideoFrame
    pub fn as_rust(&self) -> &VideoFrame {
        &self.inner
    }

    /// Convert to owned Rust VideoFrame
    pub fn into_rust(self) -> VideoFrame {
        self.inner
    }
}
