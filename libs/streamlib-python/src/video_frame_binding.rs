// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for VideoFrame.

use pyo3::prelude::*;
use streamlib::VideoFrame;

use crate::gl_context_binding::PyGlContext;
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

    /// IOSurface ID for cross-framework sharing.
    ///
    /// Use this ID to import the texture into other frameworks like SceneKit or Metal.
    /// Call `IOSurfaceLookup(id)` from PyObjC to get the IOSurface handle.
    ///
    /// Returns Some(id) on macOS if available, None on other platforms.
    #[getter]
    fn iosurface_id(&self) -> Option<u32> {
        self.inner.iosurface_id()
    }

    /// Bind this frame's texture to an OpenGL texture and return the GL texture ID (experimental).
    ///
    /// This enables interop with OpenGL-based libraries like skia-python.
    /// The GL context must be current before calling this method.
    ///
    /// Args:
    ///     gl_ctx: The GlContext from ctx.gpu._experimental_gl_context()
    ///
    /// Returns:
    ///     The OpenGL texture ID (GLuint)
    ///
    /// Example:
    ///     gl_ctx = ctx.gpu._experimental_gl_context()
    ///     gl_ctx.make_current()
    ///     frame = ctx.input("video_in").get()
    ///     gl_tex_id = frame._experimental_gl_texture_id(gl_ctx)
    fn _experimental_gl_texture_id(&self, gl_ctx: &PyGlContext) -> PyResult<u32> {
        let binding = self
            .inner
            .gl_texture_binding(&mut gl_ctx.lock())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))?;

        Ok(binding.texture_id)
    }

    /// Get the OpenGL texture target for this frame's texture (experimental).
    ///
    /// Returns GL_TEXTURE_RECTANGLE (0x84F5) for IOSurface-backed textures on macOS.
    /// Use this when constructing skia.GrGLTextureInfo.
    fn _experimental_gl_texture_target(&self) -> u32 {
        streamlib::gl_constants::GL_TEXTURE_RECTANGLE
    }

    fn __repr__(&self) -> String {
        format!(
            "VideoFrame({}x{}, frame={}, timestamp_ns={})",
            self.inner.width, self.inner.height, self.inner.frame_number, self.inner.timestamp_ns
        )
    }
}
