//! Python wrappers for wgpu objects that delegate to Rust
//!
//! These classes provide a wgpu-py-like API for Python code while using
//! the shared Rust GpuContext under the hood. This enables zero-copy
//! texture sharing between Rust and Python processors.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyBytes};
use crate::core::GpuContext;

// ============================================================================
// Helper functions for parsing Python dicts to wgpu types
// ============================================================================

/// Parse BufferUsage from Python wgpu.BufferUsage flags
fn parse_buffer_usage(py: Python<'_>, usage_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::BufferUsages> {
    // Try to get as integer
    let usage_int: u32 = usage_obj.extract()?;
    Ok(wgpu::BufferUsages::from_bits(usage_int).unwrap_or(wgpu::BufferUsages::empty()))
}

/// Parse ShaderStage from Python wgpu.ShaderStage
fn parse_shader_stage(py: Python<'_>, stage_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::ShaderStages> {
    let stage_int: u32 = stage_obj.extract()?;
    Ok(wgpu::ShaderStages::from_bits(stage_int).unwrap_or(wgpu::ShaderStages::empty()))
}

/// Parse TextureSampleType from Python dict
fn parse_texture_sample_type(py: Python<'_>, dict: &Bound<'_, PyDict>) -> PyResult<wgpu::TextureSampleType> {
    // For now, default to float
    Ok(wgpu::TextureSampleType::Float { filterable: true })
}

/// Parse TextureViewDimension from Python
fn parse_texture_view_dimension(py: Python<'_>, dim_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::TextureViewDimension> {
    // For now, default to D2
    Ok(wgpu::TextureViewDimension::D2)
}

/// Parse StorageTextureAccess from Python
fn parse_storage_texture_access(py: Python<'_>, access_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::StorageTextureAccess> {
    // For now, default to WriteOnly
    Ok(wgpu::StorageTextureAccess::WriteOnly)
}

/// Parse TextureFormat from Python
fn parse_texture_format(py: Python<'_>, format_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::TextureFormat> {
    // For now, default to Rgba8Unorm
    Ok(wgpu::TextureFormat::Rgba8Unorm)
}

/// Parse BufferBindingType from Python dict
fn parse_buffer_binding_type(_py: Python<'_>) -> PyResult<wgpu::BufferBindingType> {
    // For now, default to Uniform
    Ok(wgpu::BufferBindingType::Uniform)
}

/// Parse BindGroupLayoutEntry from Python dict
fn parse_bind_group_layout_entry(py: Python<'_>, entry_dict: &Bound<'_, PyDict>) -> PyResult<wgpu::BindGroupLayoutEntry> {
    let binding: u32 = entry_dict.get_item("binding")?.unwrap().extract()?;
    let visibility = parse_shader_stage(py, &entry_dict.get_item("visibility")?.unwrap())?;

    // Check which binding type is present
    if let Some(texture_dict) = entry_dict.get_item("texture")? {
        let texture_dict = texture_dict.downcast::<PyDict>()?;
        let sample_type = parse_texture_sample_type(py, texture_dict)?;
        let view_dimension = parse_texture_view_dimension(py, &texture_dict.get_item("view_dimension")?.unwrap())?;

        return Ok(wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension,
                multisampled: false,
            },
            count: None,
        });
    }

    if let Some(storage_dict) = entry_dict.get_item("storage_texture")? {
        let storage_dict = storage_dict.downcast::<PyDict>()?;
        let access = parse_storage_texture_access(py, &storage_dict.get_item("access")?.unwrap())?;
        let format = parse_texture_format(py, &storage_dict.get_item("format")?.unwrap())?;
        let view_dimension = parse_texture_view_dimension(py, &storage_dict.get_item("view_dimension")?.unwrap())?;

        return Ok(wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::StorageTexture {
                access,
                format,
                view_dimension,
            },
            count: None,
        });
    }

    if let Some(_buffer_dict) = entry_dict.get_item("buffer")? {
        return Ok(wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }

    Err(pyo3::exceptions::PyValueError::new_err("Unknown binding type"))
}

// ============================================================================
// Opaque handle wrappers for wgpu objects
// ============================================================================

#[pyclass(name = "WgpuShaderModule", module = "streamlib")]
pub struct PyWgpuShaderModule {
    handle: usize,
}

#[pymethods]
impl PyWgpuShaderModule {
    fn __repr__(&self) -> String {
        format!("WgpuShaderModule(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuBuffer", module = "streamlib")]
pub struct PyWgpuBuffer {
    handle: usize,
}

#[pymethods]
impl PyWgpuBuffer {
    fn __repr__(&self) -> String {
        format!("WgpuBuffer(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuBindGroupLayout", module = "streamlib")]
pub struct PyWgpuBindGroupLayout {
    handle: usize,
}

#[pymethods]
impl PyWgpuBindGroupLayout {
    fn __repr__(&self) -> String {
        format!("WgpuBindGroupLayout(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuPipelineLayout", module = "streamlib")]
pub struct PyWgpuPipelineLayout {
    handle: usize,
}

#[pymethods]
impl PyWgpuPipelineLayout {
    fn __repr__(&self) -> String {
        format!("WgpuPipelineLayout(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuComputePipeline", module = "streamlib")]
pub struct PyWgpuComputePipeline {
    handle: usize,
    // Store bind group layout handle for later use
    bind_group_layout_handle: usize,
}

#[pymethods]
impl PyWgpuComputePipeline {
    /// Get the bind group layout (Python code needs this)
    #[getter]
    fn _bind_group_layout(&self, py: Python<'_>) -> PyResult<Py<PyWgpuBindGroupLayout>> {
        Py::new(py, PyWgpuBindGroupLayout {
            handle: self.bind_group_layout_handle,
        })
    }

    fn __repr__(&self) -> String {
        format!("WgpuComputePipeline(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuBindGroup", module = "streamlib")]
pub struct PyWgpuBindGroup {
    handle: usize,
}

#[pymethods]
impl PyWgpuBindGroup {
    fn __repr__(&self) -> String {
        format!("WgpuBindGroup(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuCommandEncoder", module = "streamlib")]
pub struct PyWgpuCommandEncoder {
    handle: usize,
    context: GpuContext,
}

#[pymethods]
impl PyWgpuCommandEncoder {
    /// Begin a compute pass
    fn begin_compute_pass(&self, py: Python<'_>) -> PyResult<Py<PyWgpuComputePass>> {
        // SAFETY: handle must be a valid CommandEncoder pointer
        unsafe {
            let encoder = &mut *(self.handle as *mut wgpu::CommandEncoder);
            let compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });

            let handle = Box::into_raw(Box::new(compute_pass)) as usize;
            Py::new(py, PyWgpuComputePass { handle })
        }
    }

    /// Finish encoding and return command buffer
    fn finish(&self, py: Python<'_>) -> PyResult<usize> {
        // SAFETY: handle must be a valid CommandEncoder pointer
        unsafe {
            // Take ownership of encoder
            let encoder = Box::from_raw(self.handle as *mut wgpu::CommandEncoder);
            let command_buffer = encoder.finish();

            // Return command buffer handle
            Ok(Box::into_raw(Box::new(command_buffer)) as usize)
        }
    }

    fn __repr__(&self) -> String {
        format!("WgpuCommandEncoder(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuComputePass", module = "streamlib")]
pub struct PyWgpuComputePass {
    handle: usize,
}

#[pymethods]
impl PyWgpuComputePass {
    /// Set the compute pipeline
    fn set_pipeline(&self, pipeline: &PyWgpuComputePipeline) -> PyResult<()> {
        unsafe {
            let pass = &mut *(self.handle as *mut wgpu::ComputePass);
            let pipeline_ref = &*(pipeline.handle as *const wgpu::ComputePipeline);
            pass.set_pipeline(pipeline_ref);
        }
        Ok(())
    }

    /// Set bind group
    fn set_bind_group(
        &self,
        index: u32,
        bind_group: &PyWgpuBindGroup,
        offsets: Option<Vec<u32>>,
    ) -> PyResult<()> {
        unsafe {
            let pass = &mut *(self.handle as *mut wgpu::ComputePass);
            let bind_group_ref = &*(bind_group.handle as *const wgpu::BindGroup);
            let offsets_slice = offsets.as_ref().map(|v| v.as_slice()).unwrap_or(&[]);
            pass.set_bind_group(index, bind_group_ref, offsets_slice);
        }
        Ok(())
    }

    /// Dispatch workgroups
    fn dispatch_workgroups(&self, x: u32, y: u32, z: u32) -> PyResult<()> {
        unsafe {
            let pass = &mut *(self.handle as *mut wgpu::ComputePass);
            pass.dispatch_workgroups(x, y, z);
        }
        Ok(())
    }

    /// End the compute pass
    fn end(&self) -> PyResult<()> {
        unsafe {
            // Take ownership and drop to end the pass
            let _pass = Box::from_raw(self.handle as *mut wgpu::ComputePass);
        }
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!("WgpuComputePass(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuTextureView", module = "streamlib")]
pub struct PyWgpuTextureView {
    handle: usize,
}

#[pymethods]
impl PyWgpuTextureView {
    fn __repr__(&self) -> String {
        format!("WgpuTextureView(handle=0x{:x})", self.handle)
    }
}

#[pyclass(name = "WgpuTexture", module = "streamlib")]
pub struct PyWgpuTexture {
    pub(crate) handle: usize,
}

#[pymethods]
impl PyWgpuTexture {
    /// Create a texture view
    fn create_view(&self, py: Python<'_>) -> PyResult<Py<PyWgpuTextureView>> {
        unsafe {
            let texture = &*(self.handle as *const wgpu::Texture);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let handle = Box::into_raw(Box::new(view)) as usize;
            Py::new(py, PyWgpuTextureView { handle })
        }
    }

    fn __repr__(&self) -> String {
        format!("WgpuTexture(handle=0x{:x})", self.handle)
    }
}

// ============================================================================
// Main device and queue wrappers
// ============================================================================

/// Python wrapper for wgpu Device
#[pyclass(name = "WgpuDevice", module = "streamlib")]
pub struct PyWgpuDevice {
    context: GpuContext,
}

#[pymethods]
impl PyWgpuDevice {
    /// Create a shader module from WGSL code
    fn create_shader_module(&self, py: Python<'_>, code: String) -> PyResult<Py<PyWgpuShaderModule>> {
        let device = self.context.device();
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(code.into()),
        });

        let handle = Box::into_raw(Box::new(shader_module)) as usize;
        Py::new(py, PyWgpuShaderModule { handle })
    }

    /// Create a buffer
    fn create_buffer(&self, py: Python<'_>, size: u64, usage: &Bound<'_, PyAny>) -> PyResult<Py<PyWgpuBuffer>> {
        let device = self.context.device();
        let usage_flags = parse_buffer_usage(py, usage)?;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: usage_flags,
            mapped_at_creation: false,
        });

        let handle = Box::into_raw(Box::new(buffer)) as usize;
        Py::new(py, PyWgpuBuffer { handle })
    }

    /// Create a bind group layout
    fn create_bind_group_layout(
        &self,
        py: Python<'_>,
        entries: Vec<Py<PyDict>>,
    ) -> PyResult<Py<PyWgpuBindGroupLayout>> {
        let device = self.context.device();

        // Parse entries
        let parsed_entries: Result<Vec<_>, _> = entries
            .iter()
            .map(|entry| {
                let entry_dict = entry.bind(py);
                parse_bind_group_layout_entry(py, entry_dict)
            })
            .collect();

        let parsed_entries = parsed_entries?;

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &parsed_entries,
        });

        let handle = Box::into_raw(Box::new(layout)) as usize;
        Py::new(py, PyWgpuBindGroupLayout { handle })
    }

    /// Create a pipeline layout
    fn create_pipeline_layout(
        &self,
        py: Python<'_>,
        bind_group_layouts: Vec<Py<PyWgpuBindGroupLayout>>,
    ) -> PyResult<Py<PyWgpuPipelineLayout>> {
        let device = self.context.device();

        // Convert handles to references
        let layout_refs: Vec<&wgpu::BindGroupLayout> = bind_group_layouts
            .iter()
            .map(|layout| unsafe { &*(layout.borrow(py).handle as *const wgpu::BindGroupLayout) })
            .collect();

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &layout_refs,
            push_constant_ranges: &[],
        });

        let handle = Box::into_raw(Box::new(pipeline_layout)) as usize;
        Py::new(py, PyWgpuPipelineLayout { handle })
    }

    /// Create a compute pipeline
    fn create_compute_pipeline(
        &self,
        py: Python<'_>,
        layout: &PyWgpuPipelineLayout,
        compute: &Bound<'_, PyDict>,
    ) -> PyResult<Py<PyWgpuComputePipeline>> {
        let device = self.context.device();

        // Parse compute stage
        let module: Py<PyWgpuShaderModule> = compute.get_item("module")?.unwrap().extract()?;
        let entry_point: String = compute.get_item("entry_point")?.unwrap().extract()?;

        let module_ref = unsafe { &*(module.borrow(py).handle as *const wgpu::ShaderModule) };
        let layout_ref = unsafe { &*(layout.handle as *const wgpu::PipelineLayout) };

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(layout_ref),
            module: module_ref,
            entry_point: Some(&entry_point),
            compilation_options: Default::default(),
            cache: None,
        });

        let handle = Box::into_raw(Box::new(pipeline)) as usize;

        // Store bind group layout handle for Python access
        // For now, assume first layout in pipeline_layout
        let bind_group_layout_handle = 0; // TODO: Get from pipeline

        Py::new(py, PyWgpuComputePipeline { handle, bind_group_layout_handle })
    }

    /// Create a bind group
    fn create_bind_group(
        &self,
        py: Python<'_>,
        layout: &PyWgpuBindGroupLayout,
        entries: Vec<Py<PyDict>>,
    ) -> PyResult<Py<PyWgpuBindGroup>> {
        let device = self.context.device();
        let layout_ref = unsafe { &*(layout.handle as *const wgpu::BindGroupLayout) };

        // Parse entries - this is complex, need to handle different resource types
        let mut bind_entries = Vec::new();

        for entry_dict in entries.iter() {
            let entry_dict = entry_dict.bind(py);
            let binding: u32 = entry_dict.get_item("binding")?.unwrap().extract()?;
            let resource_obj = entry_dict.get_item("resource")?.unwrap();

            // Check resource type
            if let Ok(texture_view) = resource_obj.extract::<Py<PyWgpuTextureView>>() {
                let view_handle = texture_view.borrow(py).handle;
                let view_ref = unsafe { &*(view_handle as *const wgpu::TextureView) };
                bind_entries.push(wgpu::BindGroupEntry {
                    binding,
                    resource: wgpu::BindingResource::TextureView(view_ref),
                });
            } else if let Ok(resource_dict) = resource_obj.downcast::<PyDict>() {
                // Buffer resource (dict with "buffer" key)
                if let Some(buffer_obj) = resource_dict.get_item("buffer")? {
                    let buffer: Py<PyWgpuBuffer> = buffer_obj.extract()?;
                    let buffer_handle = buffer.borrow(py).handle;
                    let buffer_ref = unsafe { &*(buffer_handle as *const wgpu::Buffer) };
                    bind_entries.push(wgpu::BindGroupEntry {
                        binding,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: buffer_ref,
                            offset: 0,
                            size: None,
                        }),
                    });
                }
            }
        }

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: layout_ref,
            entries: &bind_entries,
        });

        let handle = Box::into_raw(Box::new(bind_group)) as usize;
        Py::new(py, PyWgpuBindGroup { handle })
    }

    /// Create a command encoder
    fn create_command_encoder(&self, py: Python<'_>) -> PyResult<Py<PyWgpuCommandEncoder>> {
        let device = self.context.device();
        let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: None,
        });

        let handle = Box::into_raw(Box::new(encoder)) as usize;
        Py::new(py, PyWgpuCommandEncoder {
            handle,
            context: self.context.clone(),
        })
    }

    fn __repr__(&self) -> String {
        "WgpuDevice(streamlib)".to_string()
    }
}

impl PyWgpuDevice {
    pub fn new(context: GpuContext) -> Self {
        Self { context }
    }
}

/// Python wrapper for wgpu Queue
#[pyclass(name = "WgpuQueue", module = "streamlib")]
pub struct PyWgpuQueue {
    context: GpuContext,
}

#[pymethods]
impl PyWgpuQueue {
    /// Write data to a buffer
    fn write_buffer(
        &self,
        buffer: &PyWgpuBuffer,
        offset: u64,
        data: &Bound<'_, PyBytes>,
    ) -> PyResult<()> {
        let queue = self.context.queue();
        let buffer_ref = unsafe { &*(buffer.handle as *const wgpu::Buffer) };

        queue.write_buffer(buffer_ref, offset, data.as_bytes());
        Ok(())
    }

    /// Submit command buffers
    fn submit(&self, command_buffers: Vec<usize>) -> PyResult<()> {
        let queue = self.context.queue();

        // SAFETY: command_buffer handles must be valid pointers
        let cmd_bufs: Vec<wgpu::CommandBuffer> = command_buffers
            .into_iter()
            .map(|handle| unsafe {
                // Take ownership of the command buffer
                *Box::from_raw(handle as *mut wgpu::CommandBuffer)
            })
            .collect();

        queue.submit(cmd_bufs);
        Ok(())
    }

    fn __repr__(&self) -> String {
        "WgpuQueue(streamlib)".to_string()
    }
}

impl PyWgpuQueue {
    pub fn new(context: GpuContext) -> Self {
        Self { context }
    }
}
