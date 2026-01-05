// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for VideoFrame.

use pyo3::prelude::*;
use streamlib::VideoFrame;

use crate::shader_handle::{PyGpuTexture, PyPooledTextureHandle};

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
    /// GPU texture handle (opaque, use with VideoFrame APIs).
    #[getter]
    fn texture(&self) -> PyGpuTexture {
        PyGpuTexture::new(self.inner.texture.clone())
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
            inner: self
                .inner
                .with_texture(texture.inner(), texture.texture_ref().format()),
        }
    }

    /// Create a new frame with a pooled texture, preserving metadata.
    ///
    /// Use this after GPU shader processing when using pooled textures.
    /// The pooled texture will be returned to the pool when the frame is dropped.
    ///
    /// Args:
    ///     pooled: PooledTexture handle from ctx.gpu.acquire_surface()
    ///
    /// Returns:
    ///     New VideoFrame with the pooled texture
    fn with_pooled_texture(&self, pooled: &mut PyPooledTextureHandle) -> PyResult<PyVideoFrame> {
        let handle = pooled.take_handle().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("PooledTexture handle already consumed")
        })?;

        Ok(PyVideoFrame {
            inner: self.inner.with_pooled_texture(handle),
        })
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
