// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Opaque handles for GPU textures and compiled shaders.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::{Arc, Mutex};
use streamlib::{GpuContext, PooledTextureHandle};

/// Opaque GPU texture handle.
///
/// Python code cannot access the underlying pixel data directly.
/// Use this handle with `ctx.gpu.dispatch()` or `frame.with_texture()`.
#[pyclass(name = "GpuTexture")]
#[derive(Clone)]
pub struct PyGpuTexture {
    texture: Arc<wgpu::Texture>,
}

impl PyGpuTexture {
    pub fn new(texture: Arc<wgpu::Texture>) -> Self {
        Self { texture }
    }

    pub fn inner(&self) -> Arc<wgpu::Texture> {
        Arc::clone(&self.texture)
    }

    pub fn texture_ref(&self) -> &wgpu::Texture {
        &self.texture
    }
}

#[pymethods]
impl PyGpuTexture {
    fn __repr__(&self) -> String {
        let size = self.texture.size();
        format!(
            "GpuTexture({}x{}, format={:?})",
            size.width,
            size.height,
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
/// and cross-library GPU memory sharing (e.g., with pygfx/wgpu-py).
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

    /// Get the texture as a GpuTexture for shader binding.
    #[getter]
    fn texture(&self) -> PyResult<PyGpuTexture> {
        self.handle
            .as_ref()
            .map(|h| PyGpuTexture::new(h.texture_arc()))
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))
    }

    /// IOSurface ID for cross-process sharing (macOS only).
    ///
    /// Use this ID to import the texture into other frameworks like pygfx/wgpu-py.
    #[cfg(target_os = "macos")]
    #[getter]
    fn iosurface_id(&self) -> PyResult<u32> {
        self.handle
            .as_ref()
            .map(|h| h.iosurface_id())
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Handle already consumed"))
    }

    fn __repr__(&self) -> String {
        match &self.handle {
            Some(h) => {
                #[cfg(target_os = "macos")]
                {
                    format!(
                        "PooledTexture({}x{}, iosurface_id={})",
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

/// Cached output texture for reuse when dimensions match.
struct CachedOutputTexture {
    texture: Arc<wgpu::Texture>,
    width: u32,
    height: u32,
}

/// Compiled compute shader handle.
///
/// Created via `ctx.gpu.compile_shader()`, used with `ctx.gpu.dispatch()`.
#[pyclass(name = "CompiledShader")]
pub struct PyCompiledShader {
    name: String,
    pipeline: Arc<wgpu::ComputePipeline>,
    bind_group_layout: Arc<wgpu::BindGroupLayout>,
    gpu_context: GpuContext,
    /// Cached output texture for reuse when dimensions match.
    cached_output: Mutex<Option<CachedOutputTexture>>,
}

impl PyCompiledShader {
    pub fn new(
        name: String,
        pipeline: Arc<wgpu::ComputePipeline>,
        bind_group_layout: Arc<wgpu::BindGroupLayout>,
        gpu_context: GpuContext,
    ) -> Self {
        Self {
            name,
            pipeline,
            bind_group_layout,
            gpu_context,
            cached_output: Mutex::new(None),
        }
    }

    /// Dispatch the shader with the given inputs and output size.
    ///
    /// Creates an output texture, sets up bind groups, and submits the compute pass.
    pub fn dispatch(
        &self,
        inputs: &Bound<'_, PyDict>,
        output_width: u32,
        output_height: u32,
    ) -> PyResult<PyGpuTexture> {
        let device = self.gpu_context.device();
        let queue = self.gpu_context.queue();

        // Reuse cached output texture if dimensions match, otherwise create new
        let output_texture = {
            let mut cache = self.cached_output.lock().unwrap();

            // Check if cached texture matches requested dimensions
            let can_reuse = cache
                .as_ref()
                .map(|c| c.width == output_width && c.height == output_height)
                .unwrap_or(false);

            if can_reuse {
                Arc::clone(&cache.as_ref().unwrap().texture)
            } else {
                // Create new texture and cache it
                let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("{}_output", self.name)),
                    size: wgpu::Extent3d {
                        width: output_width,
                        height: output_height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                }));

                *cache = Some(CachedOutputTexture {
                    texture: Arc::clone(&texture),
                    width: output_width,
                    height: output_height,
                });

                texture
            }
        };

        // Extract input texture from dict
        // For now, we expect a single "input_texture" key
        let input_texture: PyGpuTexture = inputs
            .get_item("input_texture")?
            .ok_or_else(|| {
                pyo3::exceptions::PyKeyError::new_err("Missing 'input_texture' in inputs dict")
            })?
            .extract()?;

        // Create texture views
        let input_view = input_texture
            .texture_ref()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = (*output_texture).create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });

        // Create bind group
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("{}_bind_group", self.name)),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
            ],
        });

        // Create command encoder and compute pass
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(&format!("{}_encoder", self.name)),
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(&format!("{}_pass", self.name)),
                timestamp_writes: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);

            // Dispatch with workgroup size of 16x16
            let workgroup_x = output_width.div_ceil(16);
            let workgroup_y = output_height.div_ceil(16);
            pass.dispatch_workgroups(workgroup_x, workgroup_y, 1);
        }

        queue.submit(std::iter::once(encoder.finish()));

        // Wait for GPU to complete shader execution before returning
        // Without this, the display may read the texture while it's still being written
        let _ = device.poll(wgpu::PollType::Wait);

        Ok(PyGpuTexture::new(output_texture))
    }
}

#[pymethods]
impl PyCompiledShader {
    fn __repr__(&self) -> String {
        format!("CompiledShader(name='{}')", self.name)
    }
}
