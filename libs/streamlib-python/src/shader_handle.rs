// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Opaque handles for GPU textures.

use pyo3::prelude::*;
use streamlib::core::rhi::StreamTexture;
use streamlib::PooledTextureHandle;

use crate::gl_context_binding::PyGlContext;

/// Opaque GPU texture handle.
///
/// Python code cannot access the underlying pixel data directly.
/// Use this handle with VideoFrame or other texture-consuming APIs.
#[pyclass(name = "GpuTexture")]
#[derive(Clone)]
pub struct PyGpuTexture {
    texture: StreamTexture,
}

impl PyGpuTexture {
    pub fn new(texture: StreamTexture) -> Self {
        Self { texture }
    }

    pub fn inner(&self) -> StreamTexture {
        self.texture.clone()
    }

    pub fn texture_ref(&self) -> &StreamTexture {
        &self.texture
    }
}

#[pymethods]
impl PyGpuTexture {
    /// Texture width in pixels.
    #[getter]
    fn width(&self) -> u32 {
        self.texture.width()
    }

    /// Texture height in pixels.
    #[getter]
    fn height(&self) -> u32 {
        self.texture.height()
    }

    /// IOSurface ID for cross-framework sharing (macOS only).
    #[getter]
    fn iosurface_id(&self) -> Option<u32> {
        self.texture.iosurface_id()
    }

    /// Bind this texture to an OpenGL texture and return the GL texture ID (experimental).
    ///
    /// This enables interop with OpenGL-based libraries like skia-python.
    /// The GL context must be current before calling this method.
    ///
    /// Args:
    ///     gl_ctx: The GlContext from ctx.gpu._experimental_gl_context()
    ///
    /// Returns:
    ///     The OpenGL texture ID (GLuint)
    fn _experimental_gl_texture_id(&self, gl_ctx: &PyGlContext) -> PyResult<u32> {
        let binding = self
            .texture
            .gl_texture_binding(&mut gl_ctx.lock())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))?;

        Ok(binding.texture_id)
    }

    /// Get the OpenGL texture target for this texture (experimental).
    ///
    /// Returns GL_TEXTURE_RECTANGLE (0x84F5) for IOSurface-backed textures on macOS.
    fn _experimental_gl_texture_target(&self) -> u32 {
        streamlib::gl_constants::GL_TEXTURE_RECTANGLE
    }

    fn __repr__(&self) -> String {
        format!(
            "GpuTexture({}x{}, format={:?})",
            self.texture.width(),
            self.texture.height(),
            self.texture.format()
        )
    }
}

/// Pooled texture handle for IOSurface-backed GPU textures.
///
/// Acquired via `ctx.gpu.acquire_surface()`. When this handle is dropped,
/// the texture is automatically returned to the pool for reuse.
///
/// On macOS, these textures are backed by IOSurface for cross-process
/// and cross-framework GPU memory sharing (e.g., with SceneKit, Metal).
#[pyclass(name = "PooledTexture")]
pub struct PyPooledTextureHandle {
    handle: Option<PooledTextureHandle>,
}

impl PyPooledTextureHandle {
    pub fn new(handle: PooledTextureHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    /// Take ownership of the inner handle (consumes it).
    pub fn take_handle(&mut self) -> Option<PooledTextureHandle> {
        self.handle.take()
    }

    /// Get a reference to the inner handle.
    pub fn handle_ref(&self) -> Option<&PooledTextureHandle> {
        self.handle.as_ref()
    }
}

#[pymethods]
impl PyPooledTextureHandle {
    /// Texture width in pixels.
    #[getter]
    fn width(&self) -> PyResult<u32> {
        self.handle
            .as_ref()
            .map(|h| h.width())
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))
    }

    /// Texture height in pixels.
    #[getter]
    fn height(&self) -> PyResult<u32> {
        self.handle
            .as_ref()
            .map(|h| h.height())
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))
    }

    /// Get the texture as a GpuTexture for use with VideoFrame.
    #[getter]
    fn texture(&self) -> PyResult<PyGpuTexture> {
        self.handle
            .as_ref()
            .map(|h| PyGpuTexture::new(h.texture_clone()))
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))
    }

    /// IOSurface ID for cross-framework sharing.
    ///
    /// Use this ID to import the texture into other frameworks like SceneKit or Metal.
    /// Call `IOSurfaceLookup(id)` from PyObjC to get the IOSurface handle.
    ///
    /// Returns Some(id) on macOS if available, None on other platforms.
    #[getter]
    fn iosurface_id(&self) -> PyResult<Option<u32>> {
        // Uses Rust core PooledTextureHandle::iosurface_id() which returns None on non-macOS
        self.handle
            .as_ref()
            .map(|h| h.iosurface_id())
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))
    }

    /// Platform-specific native handle dict for cross-framework sharing.
    ///
    /// Returns a dict with platform-specific keys:
    /// - macOS: {"iosurface_id": <u32>} (if available)
    /// - Linux: {"dmabuf_fd": <i32>} (when implemented)
    /// - Windows: {"dxgi_shared_handle": <u64>} (when implemented)
    ///
    /// Use this when passing textures to external libraries (pygfx, wgpu-py)
    /// that can handle multiple platform sharing mechanisms.
    #[getter]
    fn native_handle(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use streamlib::NativeTextureHandle;

        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))?;

        let dict = pyo3::types::PyDict::new(py);

        // Use the Rust core native_handle() which returns the platform-appropriate variant
        if let Some(native) = handle.native_handle() {
            match native {
                NativeTextureHandle::IOSurface { id } => {
                    dict.set_item("iosurface_id", id)?;
                }
                NativeTextureHandle::DmaBuf { fd } => {
                    dict.set_item("dmabuf_fd", fd)?;
                }
                NativeTextureHandle::DxgiSharedHandle { handle } => {
                    dict.set_item("dxgi_shared_handle", handle)?;
                }
            }
        }

        Ok(dict.into())
    }

    /// Bind this texture to an OpenGL texture and return the GL texture ID (experimental).
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
    ///     output = ctx.gpu.acquire_surface(1920, 1080)
    ///     gl_tex_id = output._experimental_gl_texture_id(gl_ctx)
    ///     # Use with skia.GrGLTextureInfo(gl_ctx.texture_target, gl_tex_id, gl_ctx.internal_format)
    fn _experimental_gl_texture_id(&self, gl_ctx: &PyGlContext) -> PyResult<u32> {
        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))?;

        let binding = handle
            .gl_texture_binding(&mut gl_ctx.lock())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))?;

        Ok(binding.texture_id)
    }

    /// Get the OpenGL texture target for this texture (experimental).
    ///
    /// Returns GL_TEXTURE_RECTANGLE (0x84F5) for IOSurface-backed textures on macOS.
    /// Use this when constructing skia.GrGLTextureInfo.
    fn _experimental_gl_texture_target(&self) -> u32 {
        streamlib::gl_constants::GL_TEXTURE_RECTANGLE
    }

    fn __repr__(&self) -> String {
        match &self.handle {
            Some(h) => {
                #[cfg(target_os = "macos")]
                {
                    format!(
                        "PooledTexture({}x{}, iosurface_id={:?})",
                        h.width(),
                        h.height(),
                        h.iosurface_id()
                    )
                }
                #[cfg(not(target_os = "macos"))]
                {
                    format!("PooledTexture({}x{})", h.width(), h.height())
                }
            }
            None => "PooledTexture(consumed)".to_string(),
        }
    }
}
