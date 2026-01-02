// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for VideoFrame.

use pyo3::prelude::*;
use std::sync::Arc;
use streamlib::VideoFrame;

use crate::shader_handle::PyGpuTexture;

/// Python-accessible VideoFrame wrapper.
///
/// VideoFrame contains a GPU texture and metadata. The texture data
/// stays on GPU; use `to_numpy()` for CPU access (expensive copy).
#[pyclass(name = "VideoFrame")]
#[derive(Clone)]
pub struct PyVideoFrame {
    inner: VideoFrame,
}

impl PyVideoFrame {
    pub fn new(frame: VideoFrame) -> Self {
        Self { inner: frame }
    }

    pub fn into_inner(self) -> VideoFrame {
        self.inner
    }

    pub fn inner(&self) -> &VideoFrame {
        &self.inner
    }
}

#[pymethods]
impl PyVideoFrame {
    /// GPU texture handle (opaque, pass to shader dispatch).
    #[getter]
    fn texture(&self) -> PyGpuTexture {
        PyGpuTexture::new(Arc::clone(&self.inner.texture))
    }

    /// Frame width in pixels.
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }

    /// Frame height in pixels.
    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }

    /// Sequential frame number.
    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    /// Monotonic timestamp in nanoseconds.
    #[getter]
    fn timestamp_ns(&self) -> i64 {
        self.inner.timestamp_ns
    }

    /// Create a new frame with a different texture, preserving metadata.
    ///
    /// Use this after GPU shader processing to create the output frame.
    fn with_texture(&self, texture: &PyGpuTexture) -> PyVideoFrame {
        PyVideoFrame {
            inner: VideoFrame {
                texture: texture.inner(),
                format: self.inner.format,
                width: self.inner.width,
                height: self.inner.height,
                frame_number: self.inner.frame_number,
                timestamp_ns: self.inner.timestamp_ns,
            },
        }
    }

    /// Copy frame data to numpy array (GPU -> CPU transfer).
    ///
    /// Returns RGBA u8 array of shape (height, width, 4).
    ///
    /// WARNING: This is expensive (~1-5ms for 1080p). Prefer GPU shaders.
    fn to_numpy<'py>(&self, _py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // Note: This requires numpy and performs a GPU->CPU copy
        // Implementation would use wgpu buffer mapping
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "to_numpy() not yet implemented - use GPU shaders for processing",
        ))
    }

    fn __repr__(&self) -> String {
        format!(
            "VideoFrame({}x{}, frame={}, timestamp_ns={})",
            self.inner.width, self.inner.height, self.inner.frame_number, self.inner.timestamp_ns
        )
    }
}
