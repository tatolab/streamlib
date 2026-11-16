// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan ray-tracing-kernel RHI: shader pipeline + descriptor set + SBT +
//! `vkCmdTraceRaysKHR` dispatch.
//!
//! Pattern matches [`crate::vulkan::rhi::VulkanComputeKernel`] / [`crate::vulkan::rhi::VulkanGraphicsKernel`]:
//! the kernel author declares stages, shader groups, bindings, and push-
//! constants once as data; the RHI reflects every stage's SPIR-V at
//! creation, validates the declarations, builds the pipeline, fetches
//! shader-group handles, and lays out the shader-binding table. From that
//! point on the user binds resources by slot via simple typed setters and
//! calls [`Self::trace_rays`].
//!
//! Binding kinds supported today: storage buffer, uniform buffer, sampled
//! texture (with a default linear-clamp sampler), storage image, top-level
//! acceleration structure. All bindings live on descriptor set 0.

#![cfg(any(target_os = "linux", target_os = "macos"))]

mod ray_tracing_kernel_bindings;
mod ray_tracing_kernel_trace_recording;
mod shader_binding_table;
mod spirv_binding_and_push_constant_validation;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrRayTracingPipelineExtensionDeviceCommands as _;
use vulkanalia_vma as vma;

use std::ffi::c_void;

use crate::core::rhi::{
    RayTracingBindingKind, RayTracingBindingSpec, RayTracingKernelDescriptor,
    RayTracingShaderGroup, RayTracingShaderStage, RayTracingShaderStageFlags, Texture,
    validate_shader_groups,
};
use crate::core::{Error, Result};

use crate::vulkan::rhi::{HostVulkanDevice, VulkanAccelerationStructure};
use shader_binding_table::{build_sbt, drop_sbt};
use spirv_binding_and_push_constant_validation::{
    validate_bindings_against_spirv, validate_push_constants_against_spirv,
};

/// Rich data backing a [`VulkanRayTracingKernel`], reached through the
/// kernel's opaque handle.
pub(crate) struct VulkanRayTracingKernelInner {
    label: String,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    bindings: Vec<RayTracingBindingSpec>,
    push_constant_size: u32,
    push_constant_stages: vk::ShaderStageFlags,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    shader_modules: Vec<vk::ShaderModule>,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// SBT buffer + regions for `vkCmdTraceRaysKHR`. Owns its own VMA
    /// allocation; freed in `Drop`.
    sbt: Sbt,
    default_sampler: Mutex<Option<vk::Sampler>>,
    pending: Mutex<PendingState>,
}

/// Owns the SBT buffer + the four `VkStridedDeviceAddressRegionKHR` values
/// passed to `vkCmdTraceRaysKHR`.
struct Sbt {
    buffer: vk::Buffer,
    allocation: Option<vma::Allocation>,
    raygen_region: vk::StridedDeviceAddressRegionKHR,
    miss_region: vk::StridedDeviceAddressRegionKHR,
    hit_region: vk::StridedDeviceAddressRegionKHR,
    callable_region: vk::StridedDeviceAddressRegionKHR,
}

struct PendingState {
    bindings: HashMap<u32, BindingResource>,
    push_constants: Vec<u8>,
}

#[derive(Clone)]
enum BindingResource {
    Buffer {
        buffer: vk::Buffer,
        size: vk::DeviceSize,
    },
    SampledImage {
        view: vk::ImageView,
        sampler: vk::Sampler,
    },
    StorageImage {
        view: vk::ImageView,
    },
    AccelerationStructure {
        handle: vk::AccelerationStructureKHR,
        // The handle clone keeps the AS alive while bound (its Clone
        // bumps the underlying Arc strong count).
        _keep_alive: VulkanAccelerationStructure,
    },
}

impl VulkanRayTracingKernelInner {
    /// Create a new ray-tracing kernel from a shader-stage list, group
    /// layout, and binding declaration.
    pub(crate) fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &RayTracingKernelDescriptor<'_>,
    ) -> Result<Self> {
        vulkan_device.refuse_without_the_ray_tracing_tier(&format!(
            "Ray-tracing kernel '{}'",
            descriptor.label
        ))?;
        let rt_props = vulkan_device.ray_tracing_pipeline_properties().ok_or_else(|| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{}': device reports RT supported but properties unavailable",
                descriptor.label
            ))
        })?;
        if descriptor.max_recursion_depth > rt_props.max_ray_recursion_depth {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': declared max_recursion_depth={} exceeds device max ({})",
                descriptor.label, descriptor.max_recursion_depth, rt_props.max_ray_recursion_depth
            )));
        }

        validate_shader_groups(descriptor.label, descriptor.stages, descriptor.groups)?;
        let reconciled_bindings = validate_bindings_against_spirv(
            descriptor,
            vulkan_device.subgroup_operation_support(),
        )?;
        validate_push_constants_against_spirv(descriptor)?;

        let device = vulkan_device.device();
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();

        // Build shader modules.
        let mut shader_modules: Vec<vk::ShaderModule> = Vec::with_capacity(descriptor.stages.len());
        for stage in descriptor.stages {
            let spirv: Vec<u32> = stage
                .spv
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let info = vk::ShaderModuleCreateInfo::builder().code(&spirv).build();
            match unsafe { device.create_shader_module(&info, None) } {
                Ok(m) => shader_modules.push(m),
                Err(e) => {
                    for m in shader_modules.drain(..) {
                        unsafe { device.destroy_shader_module(m, None) };
                    }
                    return Err(Error::GpuError(format!(
                        "Ray-tracing kernel '{}': failed to create shader module for stage {:?}: {e}",
                        descriptor.label, stage.stage
                    )));
                }
            }
        }

        // Stage-create-info: must outlive the pipeline-create call.
        let entry_points: Vec<std::ffi::CString> = descriptor
            .stages
            .iter()
            .map(|s| std::ffi::CString::new(s.entry_point).unwrap_or_default())
            .collect();
        let stage_infos: Vec<vk::PipelineShaderStageCreateInfo> = descriptor
            .stages
            .iter()
            .zip(shader_modules.iter())
            .zip(entry_points.iter())
            .map(|((stage, module), entry)| {
                vk::PipelineShaderStageCreateInfo::builder()
                    .stage(stage_to_vk(stage.stage))
                    .module(*module)
                    .name(entry.as_bytes_with_nul())
                    .build()
            })
            .collect();

        // Group create infos (one per declared group).
        let group_infos: Vec<vk::RayTracingShaderGroupCreateInfoKHR> =
            descriptor.groups.iter().map(group_to_vk).collect();

        // Descriptor-set layout + pool + set (one set, mirrors compute kernel's shape).
        let descriptor_set_layout = match create_descriptor_set_layout(device, descriptor.bindings)
        {
            Ok(l) => l,
            Err(e) => {
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(e);
            }
        };

        let pipeline_layout = match create_pipeline_layout(
            device,
            descriptor_set_layout,
            descriptor.push_constants.size,
            descriptor.push_constants.stages,
        ) {
            Ok(l) => l,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(e);
            }
        };

        // Build the RT pipeline.
        let pipeline_info = vk::RayTracingPipelineCreateInfoKHR::builder()
            .stages(&stage_infos)
            .groups(&group_infos)
            .max_pipeline_ray_recursion_depth(descriptor.max_recursion_depth)
            .layout(pipeline_layout)
            .build();
        let pipeline_result = unsafe {
            device.create_ray_tracing_pipelines_khr(
                vk::DeferredOperationKHR::null(),
                vk::PipelineCache::null(),
                &[pipeline_info],
                None,
            )
        };
        let pipeline = match pipeline_result {
            Ok(pipelines) => pipelines.0[0],
            Err(e) => {
                unsafe {
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(Error::GpuError(format!(
                    "Ray-tracing kernel '{}': vkCreateRayTracingPipelinesKHR failed: {e}",
                    descriptor.label
                )));
            }
        };

        // Fetch shader-group handles + build SBT.
        let sbt = match build_sbt(vulkan_device, descriptor, pipeline, &rt_props) {
            Ok(s) => s,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(e);
            }
        };

        // Descriptor pool + set.
        let descriptor_pool = match create_descriptor_pool(device, descriptor.bindings) {
            Ok(p) => p,
            Err(e) => {
                drop_sbt(&sbt, vulkan_device);
                unsafe {
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(e);
            }
        };
        let descriptor_set =
            match allocate_descriptor_set(device, descriptor_pool, descriptor_set_layout) {
                Ok(s) => s,
                Err(e) => {
                    drop_sbt(&sbt, vulkan_device);
                    unsafe {
                        device.destroy_descriptor_pool(descriptor_pool, None);
                        device.destroy_pipeline(pipeline, None);
                        device.destroy_pipeline_layout(pipeline_layout, None);
                        device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    }
                    for m in shader_modules.drain(..) {
                        unsafe { device.destroy_shader_module(m, None) };
                    }
                    return Err(e);
                }
            };

        // Command pool + buffer + fence.
        let command_pool = match create_command_pool(device, queue_family_index) {
            Ok(p) => p,
            Err(e) => {
                drop_sbt(&sbt, vulkan_device);
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(e);
            }
        };
        let command_buffer = match allocate_command_buffer(device, command_pool) {
            Ok(c) => c,
            Err(e) => {
                drop_sbt(&sbt, vulkan_device);
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(e);
            }
        };

        // Pre-signaled so the first dispatch can wait+reset without hanging.
        let fence_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED)
            .build();
        let fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                drop_sbt(&sbt, vulkan_device);
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                for m in shader_modules.drain(..) {
                    unsafe { device.destroy_shader_module(m, None) };
                }
                return Err(Error::GpuError(format!(
                    "Ray-tracing kernel '{}': fence creation failed: {e}",
                    descriptor.label
                )));
            }
        };

        Ok(Self {
            label: descriptor.label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            device: device.clone(),
            queue,
            bindings: reconciled_bindings,
            push_constant_size: descriptor.push_constants.size,
            push_constant_stages: stage_flags_to_vk(descriptor.push_constants.stages),
            pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            shader_modules,
            command_pool,
            command_buffer,
            fence,
            sbt,
            default_sampler: Mutex::new(None),
            pending: Mutex::new(PendingState {
                bindings: HashMap::new(),
                push_constants: Vec::new(),
            }),
        })
    }
}

impl Drop for VulkanRayTracingKernelInner {
    fn drop(&mut self) {
        unsafe {
            // Queue-mutex-guarded wait, not raw device_wait_idle (see
            // HostVulkanDevice::wait_idle — concurrent setup races otherwise).
            let _ = self.vulkan_device.wait_idle();
            if let Some(sampler) = self.default_sampler.lock().take() {
                self.device.destroy_sampler(sampler, None);
            }
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            for module in self.shader_modules.drain(..) {
                self.device.destroy_shader_module(module, None);
            }
            if let Some(allocation) = self.sbt.allocation.take() {
                self.vulkan_device
                    .allocator()
                    .destroy_buffer(self.sbt.buffer, allocation);
            }
        }
    }
}

unsafe impl Send for VulkanRayTracingKernelInner {}
unsafe impl Sync for VulkanRayTracingKernelInner {}

impl std::fmt::Debug for VulkanRayTracingKernelInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanRayTracingKernelInner")
            .field("label", &self.label)
            .field("bindings", &self.bindings)
            .field("push_constant_size", &self.push_constant_size)
            .finish()
    }
}

// =============================================================================
// Public handle
// =============================================================================

/// Ray-tracing kernel — a layout-stable `#[repr(C)]` handle. Mirrors the
/// compute kernel shape.
#[repr(C)]
pub struct VulkanRayTracingKernel {
    /// Opaque handle to the host's `Arc<VulkanRayTracingKernelInner>`.
    pub(crate) handle: *const c_void,
    /// Cached push-constant size in bytes. Set at construction.
    pub(crate) cached_push_constant_size: u32,
    /// Reserved padding so the struct stays 8-byte aligned and the
    /// trailing 4 bytes are deterministic.
    pub(crate) _reserved_padding: u32,
}

unsafe impl Send for VulkanRayTracingKernel {}
unsafe impl Sync for VulkanRayTracingKernel {}

impl VulkanRayTracingKernel {
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &RayTracingKernelDescriptor<'_>,
    ) -> Result<Self> {
        let inner = VulkanRayTracingKernelInner::new(vulkan_device, descriptor)?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    pub(crate) fn from_arc_into_raw(arc: Arc<VulkanRayTracingKernelInner>) -> Self {
        let cached_push_constant_size = arc.push_constant_size();
        let handle = Arc::into_raw(arc) as *const c_void;
        Self {
            handle,
            cached_push_constant_size,
            _reserved_padding: 0,
        }
    }

    pub(crate) fn host_inner(&self) -> &VulkanRayTracingKernelInner {
        unsafe { &*(self.handle as *const VulkanRayTracingKernelInner) }
    }

    pub fn set_acceleration_structure(
        &self,
        binding: u32,
        tlas: &VulkanAccelerationStructure,
    ) -> Result<()> {
        self.host_inner().set_acceleration_structure(binding, tlas)
    }

    /// Bind a [`crate::core::rhi::PixelBuffer`]-shaped storage
    /// buffer (SSBO) at `binding`. Per-input-type concrete shape
    /// (no generic) so the cdylib path can dispatch via the
    /// matching typed plugin ABI slot (`set_storage_buffer_pixel`)
    /// without a kind discriminator. Mirrors
    /// `VulkanComputeKernel::set_storage_buffer_pixel`.
    pub fn set_storage_buffer_pixel(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<()> {
        self.host_inner().set_storage_buffer(binding, buffer)
    }

    /// Bind a raw-bytes [`crate::core::rhi::StorageBuffer`] at
    /// `binding`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_storage_buffer_storage(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<()> {
        self.host_inner().set_storage_buffer(binding, buffer)
    }

    /// Bind a [`crate::core::rhi::UniformBuffer`] (UBO) at
    /// `binding`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_uniform_buffer(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::UniformBuffer,
    ) -> Result<()> {
        self.host_inner().set_uniform_buffer(binding, buffer)
    }

    pub fn set_sampled_texture(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.host_inner().set_sampled_texture(binding, texture)
    }

    pub fn set_storage_image(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.host_inner().set_storage_image(binding, texture)
    }

    pub fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        self.host_inner().set_push_constants(bytes)
    }

    pub fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        self.host_inner().set_push_constants_value(value)
    }

    pub fn trace_rays(&self, width: u32, height: u32, depth: u32) -> Result<()> {
        self.host_inner().trace_rays(width, height, depth)
    }

    /// Kernel's declared binding shape. Mode-routed: host-mode reads
    /// directly from `host_inner`; cdylib-mode dispatches through the
    /// v3 `bindings` slot on the per-type methods vtable. On cdylib-
    /// side plugin ABI errors the method emits a `tracing::warn` and returns
    /// an empty Vec.
    pub fn bindings(&self) -> Vec<RayTracingBindingSpec> {
        self.host_inner().bindings().to_vec()
    }

    /// Push-constant range size in bytes. Cached POD.
    pub fn push_constant_size(&self) -> u32 {
        self.cached_push_constant_size
    }
}

impl Clone for VulkanRayTracingKernel {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanRayTracingKernelInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::increment_strong_count(self.handle as *const VulkanRayTracingKernelInner);
            }
        }
        Self {
            handle: self.handle,
            cached_push_constant_size: self.cached_push_constant_size,
            _reserved_padding: self._reserved_padding,
        }
    }
}

impl Drop for VulkanRayTracingKernel {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanRayTracingKernelInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const VulkanRayTracingKernelInner);
            }
        }
    }
}

impl std::fmt::Debug for VulkanRayTracingKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanRayTracingKernel").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod plugin_abi_object_layout_tests {
    use super::*;

    #[test]
    fn vulkan_ray_tracing_kernel_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VulkanRayTracingKernel>();
    }
}

fn create_descriptor_set_layout(
    device: &vulkanalia::Device,
    bindings: &[RayTracingBindingSpec],
) -> Result<vk::DescriptorSetLayout> {
    let layout_bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .map(|spec| {
            vk::DescriptorSetLayoutBinding::builder()
                .binding(spec.binding)
                .descriptor_type(descriptor_kind_to_vk(spec.kind))
                .descriptor_count(1)
                .stage_flags(stage_flags_to_vk(spec.stages))
                .build()
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::builder()
        .bindings(&layout_bindings)
        .build();
    unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("RT descriptor-set-layout creation failed: {e}")))
}

fn create_pipeline_layout(
    device: &vulkanalia::Device,
    set_layout: vk::DescriptorSetLayout,
    push_constant_size: u32,
    push_stages: RayTracingShaderStageFlags,
) -> Result<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push_constant_ranges: Vec<vk::PushConstantRange> = if push_constant_size > 0 {
        vec![
            vk::PushConstantRange::builder()
                .stage_flags(stage_flags_to_vk(push_stages))
                .offset(0)
                .size(push_constant_size)
                .build(),
        ]
    } else {
        Vec::new()
    };
    let info = vk::PipelineLayoutCreateInfo::builder()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constant_ranges)
        .build();
    unsafe { device.create_pipeline_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("RT pipeline-layout creation failed: {e}")))
}

fn create_descriptor_pool(
    device: &vulkanalia::Device,
    bindings: &[RayTracingBindingSpec],
) -> Result<vk::DescriptorPool> {
    let mut counts: HashMap<vk::DescriptorType, u32> = HashMap::new();
    for spec in bindings {
        *counts.entry(descriptor_kind_to_vk(spec.kind)).or_insert(0) += 1;
    }
    let pool_sizes: Vec<vk::DescriptorPoolSize> = counts
        .into_iter()
        .map(|(ty, count)| {
            vk::DescriptorPoolSize::builder()
                .type_(ty)
                .descriptor_count(count)
                .build()
        })
        .collect();
    let info = vk::DescriptorPoolCreateInfo::builder()
        .max_sets(1)
        .pool_sizes(&pool_sizes)
        .build();
    unsafe { device.create_descriptor_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("RT descriptor-pool creation failed: {e}")))
}

fn allocate_descriptor_set(
    device: &vulkanalia::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
) -> Result<vk::DescriptorSet> {
    let set_layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::builder()
        .descriptor_pool(pool)
        .set_layouts(&set_layouts)
        .build();
    let sets = unsafe { device.allocate_descriptor_sets(&info) }
        .map_err(|e| Error::GpuError(format!("RT descriptor-set allocation failed: {e}")))?;
    Ok(sets[0])
}

fn create_command_pool(
    device: &vulkanalia::Device,
    queue_family_index: u32,
) -> Result<vk::CommandPool> {
    let info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .build();
    unsafe { device.create_command_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("RT command-pool creation failed: {e}")))
}

fn allocate_command_buffer(
    device: &vulkanalia::Device,
    pool: vk::CommandPool,
) -> Result<vk::CommandBuffer> {
    let info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let buffers = unsafe { device.allocate_command_buffers(&info) }
        .map_err(|e| Error::GpuError(format!("RT command-buffer allocation failed: {e}")))?;
    Ok(buffers[0])
}

fn descriptor_kind_to_vk(kind: RayTracingBindingKind) -> vk::DescriptorType {
    match kind {
        RayTracingBindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        RayTracingBindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        RayTracingBindingKind::SampledTexture => vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        RayTracingBindingKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
        RayTracingBindingKind::AccelerationStructure => {
            vk::DescriptorType::ACCELERATION_STRUCTURE_KHR
        }
    }
}

fn stage_to_vk(stage: RayTracingShaderStage) -> vk::ShaderStageFlags {
    match stage {
        RayTracingShaderStage::RayGen => vk::ShaderStageFlags::RAYGEN_KHR,
        RayTracingShaderStage::Miss => vk::ShaderStageFlags::MISS_KHR,
        RayTracingShaderStage::ClosestHit => vk::ShaderStageFlags::CLOSEST_HIT_KHR,
        RayTracingShaderStage::AnyHit => vk::ShaderStageFlags::ANY_HIT_KHR,
        RayTracingShaderStage::Intersection => vk::ShaderStageFlags::INTERSECTION_KHR,
        RayTracingShaderStage::Callable => vk::ShaderStageFlags::CALLABLE_KHR,
    }
}

fn stage_to_stage_flag(stage: RayTracingShaderStage) -> RayTracingShaderStageFlags {
    match stage {
        RayTracingShaderStage::RayGen => RayTracingShaderStageFlags::RAYGEN,
        RayTracingShaderStage::Miss => RayTracingShaderStageFlags::MISS,
        RayTracingShaderStage::ClosestHit => RayTracingShaderStageFlags::CLOSEST_HIT,
        RayTracingShaderStage::AnyHit => RayTracingShaderStageFlags::ANY_HIT,
        RayTracingShaderStage::Intersection => RayTracingShaderStageFlags::INTERSECTION,
        RayTracingShaderStage::Callable => RayTracingShaderStageFlags::CALLABLE,
    }
}

fn stage_flags_to_vk(flags: RayTracingShaderStageFlags) -> vk::ShaderStageFlags {
    let mut out = vk::ShaderStageFlags::empty();
    if flags.contains(RayTracingShaderStageFlags::RAYGEN) {
        out |= vk::ShaderStageFlags::RAYGEN_KHR;
    }
    if flags.contains(RayTracingShaderStageFlags::MISS) {
        out |= vk::ShaderStageFlags::MISS_KHR;
    }
    if flags.contains(RayTracingShaderStageFlags::CLOSEST_HIT) {
        out |= vk::ShaderStageFlags::CLOSEST_HIT_KHR;
    }
    if flags.contains(RayTracingShaderStageFlags::ANY_HIT) {
        out |= vk::ShaderStageFlags::ANY_HIT_KHR;
    }
    if flags.contains(RayTracingShaderStageFlags::INTERSECTION) {
        out |= vk::ShaderStageFlags::INTERSECTION_KHR;
    }
    if flags.contains(RayTracingShaderStageFlags::CALLABLE) {
        out |= vk::ShaderStageFlags::CALLABLE_KHR;
    }
    out
}

fn group_to_vk(group: &RayTracingShaderGroup) -> vk::RayTracingShaderGroupCreateInfoKHR {
    let unused = vk::SHADER_UNUSED_KHR;
    match *group {
        RayTracingShaderGroup::General { general } => {
            vk::RayTracingShaderGroupCreateInfoKHR::builder()
                .type_(vk::RayTracingShaderGroupTypeKHR::GENERAL)
                .general_shader(general)
                .closest_hit_shader(unused)
                .any_hit_shader(unused)
                .intersection_shader(unused)
                .build()
        }
        RayTracingShaderGroup::TrianglesHit {
            closest_hit,
            any_hit,
        } => vk::RayTracingShaderGroupCreateInfoKHR::builder()
            .type_(vk::RayTracingShaderGroupTypeKHR::TRIANGLES_HIT_GROUP)
            .general_shader(unused)
            .closest_hit_shader(closest_hit.unwrap_or(unused))
            .any_hit_shader(any_hit.unwrap_or(unused))
            .intersection_shader(unused)
            .build(),
        RayTracingShaderGroup::ProceduralHit {
            intersection,
            closest_hit,
            any_hit,
        } => vk::RayTracingShaderGroupCreateInfoKHR::builder()
            .type_(vk::RayTracingShaderGroupTypeKHR::PROCEDURAL_HIT_GROUP)
            .general_shader(unused)
            .closest_hit_shader(closest_hit.unwrap_or(unused))
            .any_hit_shader(any_hit.unwrap_or(unused))
            .intersection_shader(intersection)
            .build(),
    }
}

#[cfg(test)]
mod tests;
