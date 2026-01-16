// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for GpuContext - thin wrapper over shared Rust GpuContext.
//!
//! IMPORTANT: Python processors NEVER own their context. This is a reference
//! to the shared GpuContext provided by the Rust runtime. All pool management,
//! caching, and resource allocation happens on the Rust side.

use pyo3::prelude::*;
use streamlib::core::rhi::{PixelFormat, TextureFormat, TextureUsages};
use streamlib::{GlContext, GpuContext, TexturePoolDescriptor};

use crate::gl_context_binding::PyGlContext;
use crate::pixel_buffer_binding::PyRhiPixelBuffer;
use crate::shader_handle::PyPooledTextureHandle;

/// Python-accessible reference to the shared GpuContext.
///
/// This is a thin wrapper that calls through to the Rust GpuContext.
/// All resource management (pools, caches) is handled on the Rust side.
///
/// Access via `ctx.gpu` in processor methods.
#[pyclass(name = "GpuContext")]
pub struct PyGpuContext {
    inner: GpuContext,
    /// Lazily-created GL context for interop (one per processor due to GL's single-threaded nature)
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

    /// Acquire a pixel buffer from the shared runtime pool.
    ///
    /// Calls through to the shared GpuContext - pools are cached by (width, height, format)
    /// at the runtime level, shared across all processors.
    ///
    /// Args:
    ///     width: Buffer width in pixels
    ///     height: Buffer height in pixels
    ///     format: Pixel format string: "bgra32", "rgba32", "argb32", "rgba64",
    ///             "nv12_video", "nv12_full", "uyvy422", "yuyv422", "gray8"
    ///
    /// Returns:
    ///     PixelBuffer ready for rendering
    ///
    /// Example:
    ///     output = ctx.gpu.acquire_pixel_buffer(1920, 1080, "bgra32")
    fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PyRhiPixelBuffer> {
        let pixel_format = match format.to_lowercase().as_str() {
            "bgra32" | "bgra" => PixelFormat::Bgra32,
            "rgba32" | "rgba" => PixelFormat::Rgba32,
            "argb32" | "argb" => PixelFormat::Argb32,
            "rgba64" => PixelFormat::Rgba64,
            "nv12_video" | "nv12_video_range" => PixelFormat::Nv12VideoRange,
            "nv12_full" | "nv12_full_range" => PixelFormat::Nv12FullRange,
            "uyvy422" | "uyvy" => PixelFormat::Uyvy422,
            "yuyv422" | "yuyv" => PixelFormat::Yuyv422,
            "gray8" | "gray" => PixelFormat::Gray8,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Unsupported pixel format '{}'. Use: bgra32, rgba32, argb32, rgba64, nv12_video, nv12_full, uyvy422, yuyv422, gray8",
                    other
                )))
            }
        };
        let buffer = self
            .inner
            .acquire_pixel_buffer(width, height, pixel_format)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(PyRhiPixelBuffer::new(buffer))
    }

    /// Get the OpenGL context for GPU interop.
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
