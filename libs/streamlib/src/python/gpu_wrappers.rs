//! Python wrappers for wgpu objects that delegate to Rust
//!
//! These classes provide a wgpu-py-like API for Python code while using
//! the shared Rust GpuContext under the hood. This enables zero-copy
//! texture sharing between Rust and Python processors.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyBytes};
use crate::core::GpuContext;

// ============================================================================
// wgpu Enum Exports (replaces wgpu-py dependency)
// ============================================================================

/// BufferUsage flags (bitflags)
#[pyclass(name = "BufferUsage", module = "streamlib")]
#[derive(Clone)]
pub struct PyBufferUsage;

#[pymethods]
impl PyBufferUsage {
    #[classattr]
    const MAP_READ: u32 = 1;
    #[classattr]
    const MAP_WRITE: u32 = 2;
    #[classattr]
    const COPY_SRC: u32 = 4;
    #[classattr]
    const COPY_DST: u32 = 8;
    #[classattr]
    const INDEX: u32 = 16;
    #[classattr]
    const VERTEX: u32 = 32;
    #[classattr]
    const UNIFORM: u32 = 64;
    #[classattr]
    const STORAGE: u32 = 128;
    #[classattr]
    const INDIRECT: u32 = 256;
    #[classattr]
    const QUERY_RESOLVE: u32 = 512;
}

/// ShaderStage flags (bitflags)
#[pyclass(name = "ShaderStage", module = "streamlib")]
#[derive(Clone)]
pub struct PyShaderStage;

#[pymethods]
impl PyShaderStage {
    #[classattr]
    const VERTEX: u32 = 1;
    #[classattr]
    const FRAGMENT: u32 = 2;
    #[classattr]
    const COMPUTE: u32 = 4;
}

/// TextureSampleType enum
#[pyclass(name = "TextureSampleType", module = "streamlib")]
#[derive(Clone)]
pub struct PyTextureSampleType;

#[pymethods]
impl PyTextureSampleType {
    #[classattr]
    const FLOAT: &'static str = "float";
    #[classattr]
    const UNFILTERABLE_FLOAT: &'static str = "unfilterable-float";
    #[classattr]
    const DEPTH: &'static str = "depth";
    #[classattr]
    const SINT: &'static str = "sint";
    #[classattr]
    const UINT: &'static str = "uint";

    // Lowercase alias for compatibility
    #[classattr]
    #[pyo3(name = "float")]
    const FLOAT_LOWER: &'static str = "float";
}

/// TextureViewDimension enum
#[pyclass(name = "TextureViewDimension", module = "streamlib")]
#[derive(Clone)]
pub struct PyTextureViewDimension;

#[pymethods]
impl PyTextureViewDimension {
    #[classattr]
    const D1: &'static str = "1d";
    #[classattr]
    const D2: &'static str = "2d";
    #[classattr]
    const D2_ARRAY: &'static str = "2d-array";
    #[classattr]
    const CUBE: &'static str = "cube";
    #[classattr]
    const CUBE_ARRAY: &'static str = "cube-array";
    #[classattr]
    const D3: &'static str = "3d";

    // Lowercase alias for compatibility
    #[classattr]
    #[pyo3(name = "d2")]
    const D2_LOWER: &'static str = "2d";
}

/// StorageTextureAccess enum
#[pyclass(name = "StorageTextureAccess", module = "streamlib")]
#[derive(Clone)]
pub struct PyStorageTextureAccess;

#[pymethods]
impl PyStorageTextureAccess {
    #[classattr]
    const WRITE_ONLY: &'static str = "write-only";
    #[classattr]
    const READ_ONLY: &'static str = "read-only";
    #[classattr]
    const READ_WRITE: &'static str = "read-write";
}

/// TextureFormat enum
#[pyclass(name = "TextureFormat", module = "streamlib")]
#[derive(Clone)]
pub struct PyTextureFormat;

#[pymethods]
impl PyTextureFormat {
    #[classattr]
    const RGBA8UNORM: &'static str = "rgba8unorm";
    #[classattr]
    const RGBA8UNORM_SRGB: &'static str = "rgba8unorm-srgb";
    #[classattr]
    const BGRA8UNORM: &'static str = "bgra8unorm";
    #[classattr]
    const BGRA8UNORM_SRGB: &'static str = "bgra8unorm-srgb";
    #[classattr]
    const RGBA16FLOAT: &'static str = "rgba16float";
    #[classattr]
    const RGBA32FLOAT: &'static str = "rgba32float";
}

/// BufferBindingType enum
#[pyclass(name = "BufferBindingType", module = "streamlib")]
#[derive(Clone)]
pub struct PyBufferBindingType;

#[pymethods]
impl PyBufferBindingType {
    #[classattr]
    const UNIFORM: &'static str = "uniform";
    #[classattr]
    const STORAGE: &'static str = "storage";
    #[classattr]
    const READ_ONLY_STORAGE: &'static str = "read-only-storage";
}

// ============================================================================
// Helper functions for parsing Python dicts to wgpu types
// ============================================================================

/// Parse BufferUsage from Python wgpu.BufferUsage flags
fn parse_buffer_usage(py: Python<'_>, usage_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::BufferUsages> {
    let usage_int: u32 = usage_obj.extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid BufferUsage value: must be an integer, got {}", e)
        ))?;

    wgpu::BufferUsages::from_bits(usage_int)
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid BufferUsage flags: 0x{:x}", usage_int)
        ))
}

/// Parse ShaderStage from Python wgpu.ShaderStage
fn parse_shader_stage(py: Python<'_>, stage_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::ShaderStages> {
    let stage_int: u32 = stage_obj.extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid ShaderStage value: must be an integer, got {}", e)
        ))?;

    wgpu::ShaderStages::from_bits(stage_int)
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid ShaderStage flags: 0x{:x}", stage_int)
        ))
}

/// Parse TextureSampleType from Python dict
fn parse_texture_sample_type(py: Python<'_>, dict: &Bound<'_, PyDict>) -> PyResult<wgpu::TextureSampleType> {
    let sample_type_str: String = dict
        .get_item("sample_type")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'sample_type' in texture dict"))?
        .extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid sample_type value: must be a string, got {}", e)
        ))?;

    match sample_type_str.as_str() {
        "float" => Ok(wgpu::TextureSampleType::Float { filterable: true }),
        "unfilterable-float" => Ok(wgpu::TextureSampleType::Float { filterable: false }),
        "depth" => Ok(wgpu::TextureSampleType::Depth),
        "sint" => Ok(wgpu::TextureSampleType::Sint),
        "uint" => Ok(wgpu::TextureSampleType::Uint),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            format!("Unknown TextureSampleType: '{}'. Valid values: float, unfilterable-float, depth, sint, uint", sample_type_str)
        ))
    }
}

/// Parse TextureViewDimension from Python
fn parse_texture_view_dimension(py: Python<'_>, dim_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::TextureViewDimension> {
    let dim_str: String = dim_obj.extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid view_dimension value: must be a string, got {}", e)
        ))?;

    match dim_str.as_str() {
        "1d" => Ok(wgpu::TextureViewDimension::D1),
        "2d" => Ok(wgpu::TextureViewDimension::D2),
        "2d-array" => Ok(wgpu::TextureViewDimension::D2Array),
        "cube" => Ok(wgpu::TextureViewDimension::Cube),
        "cube-array" => Ok(wgpu::TextureViewDimension::CubeArray),
        "3d" => Ok(wgpu::TextureViewDimension::D3),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            format!("Unknown TextureViewDimension: '{}'. Valid values: 1d, 2d, 2d-array, cube, cube-array, 3d", dim_str)
        ))
    }
}

/// Parse StorageTextureAccess from Python
fn parse_storage_texture_access(py: Python<'_>, access_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::StorageTextureAccess> {
    let access_str: String = access_obj.extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid access value: must be a string, got {}", e)
        ))?;

    match access_str.as_str() {
        "write-only" => Ok(wgpu::StorageTextureAccess::WriteOnly),
        "read-only" => Ok(wgpu::StorageTextureAccess::ReadOnly),
        "read-write" => Ok(wgpu::StorageTextureAccess::ReadWrite),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            format!("Unknown StorageTextureAccess: '{}'. Valid values: write-only, read-only, read-write", access_str)
        ))
    }
}

/// Parse TextureFormat from Python
fn parse_texture_format(py: Python<'_>, format_obj: &Bound<'_, PyAny>) -> PyResult<wgpu::TextureFormat> {
    let format_str: String = format_obj.extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid format value: must be a string, got {}", e)
        ))?;

    match format_str.as_str() {
        "rgba8unorm" => Ok(wgpu::TextureFormat::Rgba8Unorm),
        "rgba8unorm-srgb" => Ok(wgpu::TextureFormat::Rgba8UnormSrgb),
        "bgra8unorm" => Ok(wgpu::TextureFormat::Bgra8Unorm),
        "bgra8unorm-srgb" => Ok(wgpu::TextureFormat::Bgra8UnormSrgb),
        "rgba16float" => Ok(wgpu::TextureFormat::Rgba16Float),
        "rgba32float" => Ok(wgpu::TextureFormat::Rgba32Float),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            format!("Unknown TextureFormat: '{}'. Supported formats: rgba8unorm, rgba8unorm-srgb, bgra8unorm, bgra8unorm-srgb, rgba16float, rgba32float", format_str)
        ))
    }
}

/// Parse BufferBindingType from Python dict
fn parse_buffer_binding_type(py: Python<'_>, buffer_dict: &Bound<'_, PyDict>) -> PyResult<wgpu::BufferBindingType> {
    let type_str: String = buffer_dict
        .get_item("type")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'type' in buffer dict"))?
        .extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid buffer type value: must be a string, got {}", e)
        ))?;

    match type_str.as_str() {
        "uniform" => Ok(wgpu::BufferBindingType::Uniform),
        "storage" => Ok(wgpu::BufferBindingType::Storage { read_only: false }),
        "read-only-storage" => Ok(wgpu::BufferBindingType::Storage { read_only: true }),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            format!("Unknown BufferBindingType: '{}'. Valid values: uniform, storage, read-only-storage", type_str)
        ))
    }
}

/// Parse BindGroupLayoutEntry from Python dict
fn parse_bind_group_layout_entry(py: Python<'_>, entry_dict: &Bound<'_, PyDict>) -> PyResult<wgpu::BindGroupLayoutEntry> {
    let binding: u32 = entry_dict
        .get_item("binding")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'binding' key in bind group layout entry"))?
        .extract()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(
            format!("Invalid 'binding' value: must be an integer, got {}", e)
        ))?;

    let visibility_obj = entry_dict
        .get_item("visibility")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'visibility' key in bind group layout entry"))?;
    let visibility = parse_shader_stage(py, &visibility_obj)?;

    // Check which binding type is present
    if let Some(texture_dict) = entry_dict.get_item("texture")? {
        let texture_dict = texture_dict.downcast::<PyDict>()
            .map_err(|e| pyo3::exceptions::PyTypeError::new_err(
                format!("'texture' must be a dict, got {}", e)
            ))?;

        let sample_type = parse_texture_sample_type(py, texture_dict)?;

        let view_dimension_obj = texture_dict
            .get_item("view_dimension")?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'view_dimension' in texture dict"))?;
        let view_dimension = parse_texture_view_dimension(py, &view_dimension_obj)?;

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
        let storage_dict = storage_dict.downcast::<PyDict>()
            .map_err(|e| pyo3::exceptions::PyTypeError::new_err(
                format!("'storage_texture' must be a dict, got {}", e)
            ))?;

        let access_obj = storage_dict
            .get_item("access")?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'access' in storage_texture dict"))?;
        let access = parse_storage_texture_access(py, &access_obj)?;

        let format_obj = storage_dict
            .get_item("format")?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'format' in storage_texture dict"))?;
        let format = parse_texture_format(py, &format_obj)?;

        let view_dimension_obj = storage_dict
            .get_item("view_dimension")?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("Missing 'view_dimension' in storage_texture dict"))?;
        let view_dimension = parse_texture_view_dimension(py, &view_dimension_obj)?;

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

    if let Some(buffer_dict) = entry_dict.get_item("buffer")? {
        let buffer_dict = buffer_dict.downcast::<PyDict>()
            .map_err(|e| pyo3::exceptions::PyTypeError::new_err(
                format!("'buffer' must be a dict, got {}", e)
            ))?;

        let buffer_type = parse_buffer_binding_type(py, buffer_dict)?;

        return Ok(wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: buffer_type,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }

    Err(pyo3::exceptions::PyValueError::new_err(
        "Unknown binding type: entry must have one of 'texture', 'storage_texture', or 'buffer' keys"
    ))
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
