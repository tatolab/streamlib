// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for GpuContext with texture pool access.

use pyo3::prelude::*;
use streamlib::core::rhi::{TextureFormat, TextureUsages};
use streamlib::{GpuContext, TexturePoolDescriptor};

use crate::shader_handle::PyPooledTextureHandle;

/// Python-accessible GpuContext for texture pool operations.
///
/// Access via `ctx.gpu` in processor methods.
#[pyclass(name = "GpuContext")]
#[derive(Clone)]
pub struct PyGpuContext {
    inner: GpuContext,
}

impl PyGpuContext {
    pub fn new(ctx: GpuContext) -> Self {
        Self { inner: ctx }
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

    fn __repr__(&self) -> String {
        format!("GpuContext({:?})", self.inner)
    }
}
