// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for GpuContext with texture pool access.

use pyo3::prelude::*;
use streamlib::core::rhi::{TextureFormat, TextureUsages};
use streamlib::{GlContext, GpuContext, TexturePoolDescriptor};

use crate::gl_context_binding::PyGlContext;
use crate::shader_handle::PyPooledTextureHandle;

/// Python-accessible GpuContext for texture pool operations.
///
/// Access via `ctx.gpu` in processor methods.
#[pyclass(name = "GpuContext")]
pub struct PyGpuContext {
    inner: GpuContext,
    /// Lazily-created GL context for interop
    gl_context: Option<PyGlContext>,
}

impl PyGpuContext {
    pub fn new(ctx: GpuContext) -> Self {
        Self {
            inner: ctx,
            gl_context: None,
        }
    }

    pub fn inner(&self) -> &GpuContext {
        &self.inner
    }
}

#[pymethods]
impl PyGpuContext {
    /// Acquire an IOSurface-backed texture from the pool.
    ///
    /// The texture is automatically returned to the pool when the handle is dropped.
    /// On macOS, use `handle.iosurface_id` to share with other frameworks like SceneKit.
    ///
    /// Args:
    ///     width: Texture width in pixels
    ///     height: Texture height in pixels
    ///     format: Texture format (optional, defaults to "rgba8")
    ///             Supported: "rgba8", "bgra8", "rgba8_srgb", "bgra8_srgb"
    ///
    /// Returns:
    ///     PooledTexture handle
    ///
    /// Example:
    ///     output = ctx.gpu.acquire_surface(1920, 1080)
    ///     # Use output.texture with VideoFrame
    ///     # output.iosurface_id for cross-framework sharing (macOS)
    #[pyo3(signature = (width, height, format=None))]
    fn acquire_surface(
        &self,
        width: u32,
        height: u32,
        format: Option<&str>,
    ) -> PyResult<PyPooledTextureHandle> {
        let texture_format = match format.unwrap_or("rgba8") {
            "rgba8" => TextureFormat::Rgba8Unorm,
            "bgra8" => TextureFormat::Bgra8Unorm,
            "rgba8_srgb" => TextureFormat::Rgba8UnormSrgb,
            "bgra8_srgb" => TextureFormat::Bgra8UnormSrgb,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Unsupported format '{}'. Use: rgba8, bgra8, rgba8_srgb, bgra8_srgb",
                    other
                )))
            }
        };

        let desc = TexturePoolDescriptor {
            width,
            height,
            format: texture_format,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::COPY_SRC,
            label: Some("python_pooled_texture"),
        };

        let handle = self
            .inner
            .acquire_texture(&desc)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))?;

        Ok(PyPooledTextureHandle::new(handle))
    }

    /// Get the OpenGL context for GPU interop (experimental).
    ///
    /// This provides access to StreamLib's OpenGL context for use with
    /// libraries like skia-python that require OpenGL.
    ///
    /// The context is created lazily on first access and cached for reuse.
    ///
    /// Example:
    ///     gl_ctx = ctx.gpu._experimental_gl_context()
    ///     gl_ctx.make_current()
    ///     skia_ctx = skia.GrDirectContext.MakeGL()
    ///
    /// Note: This is an experimental API and may change in future versions.
    fn _experimental_gl_context(&mut self) -> PyResult<PyGlContext> {
        if let Some(ref gl_ctx) = self.gl_context {
            return Ok(gl_ctx.clone());
        }

        // Create new GL context
        let gl_ctx = GlContext::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))?;

        let py_gl_ctx = PyGlContext::new(gl_ctx);
        self.gl_context = Some(py_gl_ctx.clone());
        Ok(py_gl_ctx)
    }

    fn __repr__(&self) -> String {
        format!("GpuContext({:?})", self.inner)
    }
}
