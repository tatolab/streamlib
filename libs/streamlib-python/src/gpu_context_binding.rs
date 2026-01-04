// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for GpuContext with shader compilation and dispatch.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use streamlib::{GpuContext, TexturePoolDescriptor};

use crate::shader_handle::{PyCompiledShader, PyGpuTexture, PyPooledTextureHandle};

/// Python-accessible GpuContext for shader compilation and dispatch.
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
    /// Compile a WGSL compute shader.
    ///
    /// The shader must have an entry point named "main" with workgroup_size(16, 16).
    ///
    /// Expected bindings:
    /// - @group(0) @binding(0) var input_texture: texture_2d<f32>;
    /// - @group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;
    ///
    /// Args:
    ///     name: Shader name for debugging/profiling
    ///     wgsl_code: WGSL shader source code
    ///
    /// Returns:
    ///     CompiledShader handle for use with dispatch()
    fn compile_shader(&self, name: &str, wgsl_code: &str) -> PyResult<PyCompiledShader> {
        let device = self.inner.device();

        // Create shader module
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(name),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(wgsl_code)),
        });

        // Create bind group layout for input texture + output storage texture
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&format!("{}_bind_group_layout", name)),
            entries: &[
                // @group(0) @binding(0) var input_texture: texture_2d<f32>;
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // @group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        // Create pipeline layout
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{}_pipeline_layout", name)),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Create compute pipeline
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&format!("{}_pipeline", name)),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        tracing::debug!("Compiled shader: {}", name);

        Ok(PyCompiledShader::new(
            name.to_string(),
            Arc::new(pipeline),
            Arc::new(bind_group_layout),
            self.inner.clone(),
        ))
    }

    /// Dispatch a compute shader.
    ///
    /// Args:
    ///     shader: Compiled shader handle from compile_shader()
    ///     inputs: Dict mapping binding names to GpuTexture handles
    ///             Currently expects {"input_texture": GpuTexture}
    ///     output_width: Output texture width in pixels
    ///     output_height: Output texture height in pixels
    ///
    /// Returns:
    ///     GpuTexture containing the shader output
    fn dispatch(
        &self,
        shader: &PyCompiledShader,
        inputs: &Bound<'_, PyDict>,
        output_width: u32,
        output_height: u32,
    ) -> PyResult<PyGpuTexture> {
        shader.dispatch(inputs, output_width, output_height)
    }

    /// Acquire an IOSurface-backed texture from the pool.
    ///
    /// The texture is automatically returned to the pool when the handle is dropped.
    /// On macOS, use `handle.iosurface_id` to share with other frameworks like pygfx.
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
    ///     # Use output.texture with shaders
    ///     # output.iosurface_id for cross-process sharing (macOS)
    #[pyo3(signature = (width, height, format=None))]
    fn acquire_surface(
        &self,
        width: u32,
        height: u32,
        format: Option<&str>,
    ) -> PyResult<PyPooledTextureHandle> {
        let texture_format = match format.unwrap_or("rgba8") {
            "rgba8" => wgpu::TextureFormat::Rgba8Unorm,
            "bgra8" => wgpu::TextureFormat::Bgra8Unorm,
            "rgba8_srgb" => wgpu::TextureFormat::Rgba8UnormSrgb,
            "bgra8_srgb" => wgpu::TextureFormat::Bgra8UnormSrgb,
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
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
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
