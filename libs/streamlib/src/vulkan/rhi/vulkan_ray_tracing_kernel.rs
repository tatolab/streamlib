// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan ray-tracing-kernel RHI: shader pipeline + descriptor set + SBT +
//! `vkCmdTraceRaysKHR` dispatch.
//!
//! Pattern matches [`super::VulkanComputeKernel`] / [`super::VulkanGraphicsKernel`]:
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

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrRayTracingPipelineExtensionDeviceCommands as _;
use vulkanalia_vma as vma;
use vma::Alloc as _;

use crate::core::rhi::{
    validate_shader_groups, RayTracingBindingKind, RayTracingBindingSpec,
    RayTracingKernelDescriptor, RayTracingShaderGroup, RayTracingShaderStage,
    RayTracingShaderStageFlags, RayTracingStage, RhiPixelBuffer, StreamTexture,
};
use crate::core::{Result, StreamError};

use super::{HostVulkanDevice, VulkanAccelerationStructure};

/// One ray-tracing kernel: pipeline + descriptor set + SBT + per-dispatch primitives.
pub struct VulkanRayTracingKernel {
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
        // The strong reference keeps the AS alive while bound.
        _keep_alive: Arc<VulkanAccelerationStructure>,
    },
}

impl VulkanRayTracingKernel {
    /// Create a new ray-tracing kernel from a shader-stage list, group
    /// layout, and binding declaration.
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &RayTracingKernelDescriptor<'_>,
    ) -> Result<Self> {
        if !vulkan_device.supports_ray_tracing_pipeline() {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': ray-tracing extensions not supported by device",
                descriptor.label
            )));
        }
        let rt_props = vulkan_device.ray_tracing_pipeline_properties().ok_or_else(|| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': device reports RT supported but properties unavailable",
                descriptor.label
            ))
        })?;
        if descriptor.max_recursion_depth > rt_props.max_ray_recursion_depth {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': declared max_recursion_depth={} exceeds device max ({})",
                descriptor.label,
                descriptor.max_recursion_depth,
                rt_props.max_ray_recursion_depth
            )));
        }

        validate_shader_groups(descriptor.label, descriptor.stages, descriptor.groups)?;
        validate_bindings_against_spirv(descriptor)?;
        validate_push_constants_against_spirv(descriptor)?;

        let device = vulkan_device.device();
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();

        // Build shader modules.
        let mut shader_modules: Vec<vk::ShaderModule> = Vec::with_capacity(descriptor.stages.len());
        for stage in descriptor.stages {
            let spirv: Vec<u32> = stage
                .spv
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let info = vk::ShaderModuleCreateInfo::builder().code(&spirv).build();
            match unsafe { device.create_shader_module(&info, None) } {
                Ok(m) => shader_modules.push(m),
                Err(e) => {
                    for m in shader_modules.drain(..) {
                        unsafe { device.destroy_shader_module(m, None) };
                    }
                    return Err(StreamError::GpuError(format!(
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
        let group_infos: Vec<vk::RayTracingShaderGroupCreateInfoKHR> = descriptor
            .groups
            .iter()
            .map(|g| group_to_vk(g))
            .collect();

        // Descriptor-set layout + pool + set (one set, mirrors compute kernel's shape).
        let descriptor_set_layout =
            match create_descriptor_set_layout(device, descriptor.bindings) {
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
                return Err(StreamError::GpuError(format!(
                    "Ray-tracing kernel '{}': vkCreateRayTracingPipelinesKHR failed: {e}",
                    descriptor.label
                )));
            }
        };

        // Fetch shader-group handles + build SBT.
        let sbt = match build_sbt(
            vulkan_device,
            descriptor,
            pipeline,
            &rt_props,
        ) {
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
        let descriptor_set = match allocate_descriptor_set(
            device,
            descriptor_pool,
            descriptor_set_layout,
        ) {
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
                return Err(StreamError::GpuError(format!(
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
            bindings: descriptor.bindings.to_vec(),
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

    /// Bind a top-level acceleration structure at `binding`. The slot must
    /// be declared as [`RayTracingBindingKind::AccelerationStructure`].
    pub fn set_acceleration_structure(
        &self,
        binding: u32,
        tlas: &Arc<VulkanAccelerationStructure>,
    ) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::AccelerationStructure)?;
        if tlas.kind() != super::AccelerationStructureKind::TopLevel {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} expects a top-level AS, got {:?}",
                self.label, binding, tlas.kind()
            )));
        }
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::AccelerationStructure {
                handle: tlas.vk_handle(),
                _keep_alive: Arc::clone(tlas),
            },
        );
        Ok(())
    }

    pub fn set_storage_buffer(&self, binding: u32, buffer: &RhiPixelBuffer) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::StorageBuffer)?;
        let (vk_buf, size) = vk_buffer_for(buffer);
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: vk_buf,
                size,
            },
        );
        Ok(())
    }

    pub fn set_uniform_buffer(&self, binding: u32, buffer: &RhiPixelBuffer) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::UniformBuffer)?;
        let (vk_buf, size) = vk_buffer_for(buffer);
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: vk_buf,
                size,
            },
        );
        Ok(())
    }

    pub fn set_sampled_texture(&self, binding: u32, texture: &StreamTexture) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::SampledTexture)?;
        let view = texture.inner.image_view()?;
        let sampler = self.default_sampler()?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::SampledImage { view, sampler },
        );
        Ok(())
    }

    pub fn set_storage_image(&self, binding: u32, texture: &StreamTexture) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::StorageImage)?;
        let view = texture.inner.image_view()?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::StorageImage { view });
        Ok(())
    }

    pub fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u32 != self.push_constant_size {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': push-constant size mismatch — got {} bytes, kernel declares {}",
                self.label,
                bytes.len(),
                self.push_constant_size
            )));
        }
        self.pending.lock().push_constants = bytes.to_vec();
        Ok(())
    }

    pub fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let bytes = unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size) };
        self.set_push_constants(bytes)
    }

    /// Run the kernel: write all staged descriptors, record bind+push+
    /// trace_rays, submit to the device queue, and wait on the kernel
    /// fence before returning.
    pub fn trace_rays(&self, width: u32, height: u32, depth: u32) -> Result<()> {
        let pending = {
            let mut guard = self.pending.lock();
            PendingState {
                bindings: std::mem::take(&mut guard.bindings),
                push_constants: std::mem::take(&mut guard.push_constants),
            }
        };

        for spec in &self.bindings {
            if !pending.bindings.contains_key(&spec.binding) {
                return Err(StreamError::GpuError(format!(
                    "Ray-tracing kernel '{}': binding {} ({:?}) not set before trace_rays",
                    self.label, spec.binding, spec.kind
                )));
            }
        }
        if self.push_constant_size > 0 && pending.push_constants.is_empty() {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': push constants not set before trace_rays",
                self.label
            )));
        }

        unsafe {
            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': wait_for_fences failed: {e}",
                        self.label
                    ))
                })?;
            self.device.reset_fences(&[self.fence]).map_err(|e| {
                StreamError::GpuError(format!(
                    "Ray-tracing kernel '{}': reset_fences failed: {e}",
                    self.label
                ))
            })?;
        }

        self.flush_descriptor_writes(&pending)?;

        unsafe {
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': reset_command_buffer failed: {e}",
                        self.label
                    ))
                })?;
            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': begin_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            self.device.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::RAY_TRACING_KHR,
                self.pipeline,
            );
            self.device.cmd_bind_descriptor_sets(
                self.command_buffer,
                vk::PipelineBindPoint::RAY_TRACING_KHR,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );
            if self.push_constant_size > 0 {
                self.device.cmd_push_constants(
                    self.command_buffer,
                    self.pipeline_layout,
                    self.push_constant_stages,
                    0,
                    &pending.push_constants,
                );
            }

            self.device.cmd_trace_rays_khr(
                self.command_buffer,
                &self.sbt.raygen_region,
                &self.sbt.miss_region,
                &self.sbt.hit_region,
                &self.sbt.callable_region,
                width,
                height,
                depth,
            );

            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': end_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();

            HostVulkanDevice::submit_to_queue(
                &self.vulkan_device,
                self.queue,
                &[submit],
                self.fence,
            )?;

            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map(|_| ())
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': wait_for_fences (post-submit) failed: {e}",
                        self.label
                    ))
                })?;
        }

        Ok(())
    }

    pub fn bindings(&self) -> &[RayTracingBindingSpec] {
        &self.bindings
    }

    pub fn push_constant_size(&self) -> u32 {
        self.push_constant_size
    }

    fn expect_kind(&self, binding: u32, expected: RayTracingBindingKind) -> Result<()> {
        let spec = self
            .bindings
            .iter()
            .find(|b| b.binding == binding)
            .ok_or_else(|| {
                StreamError::GpuError(format!(
                    "Ray-tracing kernel '{}': binding {} not declared",
                    self.label, binding
                ))
            })?;
        if spec.kind != expected {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} declared as {:?}, but {:?} was set",
                self.label, binding, spec.kind, expected
            )));
        }
        Ok(())
    }

    fn default_sampler(&self) -> Result<vk::Sampler> {
        let mut guard = self.default_sampler.lock();
        if let Some(s) = *guard {
            return Ok(s);
        }
        let info = vk::SamplerCreateInfo::builder()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .min_lod(0.0)
            .max_lod(0.0)
            .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK)
            .unnormalized_coordinates(false)
            .build();
        let sampler = unsafe { self.device.create_sampler(&info, None) }.map_err(|e| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': failed to create default sampler: {e}",
                self.label
            ))
        })?;
        *guard = Some(sampler);
        Ok(sampler)
    }

    fn flush_descriptor_writes(&self, pending: &PendingState) -> Result<()> {
        let mut buffer_infos: Vec<vk::DescriptorBufferInfo> = Vec::with_capacity(self.bindings.len());
        let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::with_capacity(self.bindings.len());
        let mut as_handles: Vec<vk::AccelerationStructureKHR> = Vec::with_capacity(self.bindings.len());
        let mut as_writes: Vec<vk::WriteDescriptorSetAccelerationStructureKHR> =
            Vec::with_capacity(self.bindings.len());

        struct Slot {
            binding: u32,
            ty: vk::DescriptorType,
            buffer_idx: Option<usize>,
            image_idx: Option<usize>,
            as_idx: Option<usize>,
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(self.bindings.len());

        for spec in &self.bindings {
            let res = pending.bindings.get(&spec.binding).expect("checked above");
            match (spec.kind, res) {
                (RayTracingBindingKind::StorageBuffer, BindingResource::Buffer { buffer, size }) => {
                    let idx = buffer_infos.len();
                    buffer_infos.push(
                        vk::DescriptorBufferInfo::builder()
                            .buffer(*buffer)
                            .offset(0)
                            .range(*size)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::STORAGE_BUFFER,
                        buffer_idx: Some(idx),
                        image_idx: None,
                        as_idx: None,
                    });
                }
                (RayTracingBindingKind::UniformBuffer, BindingResource::Buffer { buffer, size }) => {
                    let idx = buffer_infos.len();
                    buffer_infos.push(
                        vk::DescriptorBufferInfo::builder()
                            .buffer(*buffer)
                            .offset(0)
                            .range(*size)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::UNIFORM_BUFFER,
                        buffer_idx: Some(idx),
                        image_idx: None,
                        as_idx: None,
                    });
                }
                (
                    RayTracingBindingKind::SampledTexture,
                    BindingResource::SampledImage { view, sampler },
                ) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .sampler(*sampler)
                            .image_view(*view)
                            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        buffer_idx: None,
                        image_idx: Some(idx),
                        as_idx: None,
                    });
                }
                (RayTracingBindingKind::StorageImage, BindingResource::StorageImage { view }) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .image_view(*view)
                            .image_layout(vk::ImageLayout::GENERAL)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::STORAGE_IMAGE,
                        buffer_idx: None,
                        image_idx: Some(idx),
                        as_idx: None,
                    });
                }
                (
                    RayTracingBindingKind::AccelerationStructure,
                    BindingResource::AccelerationStructure { handle, .. },
                ) => {
                    let idx = as_handles.len();
                    as_handles.push(*handle);
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::ACCELERATION_STRUCTURE_KHR,
                        buffer_idx: None,
                        image_idx: None,
                        as_idx: Some(idx),
                    });
                }
                _ => {
                    return Err(StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': binding {} kind/resource mismatch (declared {:?})",
                        self.label, spec.binding, spec.kind
                    )));
                }
            }
        }

        // Pre-build the AS-write extension structs (they reference into
        // `as_handles` which must outlive the update_descriptor_sets call).
        for slot in &slots {
            if let Some(idx) = slot.as_idx {
                let as_write = vk::WriteDescriptorSetAccelerationStructureKHR::builder()
                    .acceleration_structures(std::slice::from_ref(&as_handles[idx]))
                    .build();
                as_writes.push(as_write);
            }
        }

        let mut as_write_iter = as_writes.iter_mut();
        let mut writes: Vec<vk::WriteDescriptorSet> = Vec::with_capacity(slots.len());
        for slot in &slots {
            let mut write = vk::WriteDescriptorSet::builder()
                .dst_set(self.descriptor_set)
                .dst_binding(slot.binding)
                .descriptor_type(slot.ty);
            if let Some(i) = slot.buffer_idx {
                write = write.buffer_info(std::slice::from_ref(&buffer_infos[i]));
            }
            if let Some(i) = slot.image_idx {
                write = write.image_info(std::slice::from_ref(&image_infos[i]));
            }
            if slot.as_idx.is_some() {
                let as_write = as_write_iter
                    .next()
                    .expect("as_writes built in lockstep with as_idx slots");
                write = write.push_next(as_write);
            }
            // ACCELERATION_STRUCTURE_KHR descriptors carry the AS handle
            // through the chained `VkWriteDescriptorSetAccelerationStructureKHR`,
            // not through buffer_info / image_info — and the
            // `buffer_info` / `image_info` setters that normally set
            // `descriptor_count` aren't called for this shape, so the
            // count stays at the builder default (0). The driver
            // silently no-ops a write with `descriptorCount == 0`,
            // which means the AS binding never updates and every ray
            // returns the miss-shader output. Set it explicitly after
            // build — the builder exposes descriptor_count only as a
            // public field on the underlying struct.
            let mut built = write.build();
            if slot.as_idx.is_some() {
                built.descriptor_count = 1;
            }
            writes.push(built);
        }

        unsafe {
            self.device
                .update_descriptor_sets(&writes, &[] as &[vk::CopyDescriptorSet]);
        }
        Ok(())
    }
}

impl Drop for VulkanRayTracingKernel {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(sampler) = self.default_sampler.lock().take() {
                self.device.destroy_sampler(sampler, None);
            }
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_pipeline_layout(self.pipeline_layout, None);
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

unsafe impl Send for VulkanRayTracingKernel {}
unsafe impl Sync for VulkanRayTracingKernel {}

impl std::fmt::Debug for VulkanRayTracingKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanRayTracingKernel")
            .field("label", &self.label)
            .field("bindings", &self.bindings)
            .field("push_constant_size", &self.push_constant_size)
            .finish()
    }
}

// ---- Validation + creation helpers --------------------------------------------

fn validate_bindings_against_spirv(descriptor: &RayTracingKernelDescriptor<'_>) -> Result<()> {
    use std::collections::BTreeMap;

    // Merge per-stage SPIR-V reflection into a single map.
    let mut merged: BTreeMap<u32, (RayTracingBindingKind, RayTracingShaderStageFlags)> =
        BTreeMap::new();

    for stage in descriptor.stages {
        let reflection = Reflection::new_from_spirv(stage.spv).map_err(|e| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': failed to reflect SPIR-V for stage {:?}: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        let sets = reflection.get_descriptor_sets().map_err(|e| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': failed to extract descriptor sets for stage {:?}: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        if sets.len() > 1 {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': only descriptor set 0 supported; stage {:?} uses sets {:?}",
                descriptor.label,
                stage.stage,
                sets.keys().collect::<Vec<_>>()
            )));
        }
        let stage_flag = stage_to_stage_flag(stage.stage);
        if let Some(set0) = sets.get(&0) {
            for (&binding, info) in set0 {
                let kind = spirv_type_to_kind(info.ty).ok_or_else(|| {
                    StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': SPIR-V binding {} in stage {:?} has unsupported descriptor type {:?}",
                        descriptor.label, binding, stage.stage, info.ty
                    ))
                })?;
                let entry = merged
                    .entry(binding)
                    .or_insert((kind, RayTracingShaderStageFlags::NONE));
                if entry.0 != kind {
                    return Err(StreamError::GpuError(format!(
                        "Ray-tracing kernel '{}': SPIR-V binding {} declared as {:?} in one stage and {:?} in another",
                        descriptor.label, binding, entry.0, kind
                    )));
                }
                entry.1 |= stage_flag;
            }
        }
    }

    // Every declared binding must exist in the merged SPIR-V map.
    for spec in descriptor.bindings {
        let (spirv_kind, spirv_stages) = merged.get(&spec.binding).ok_or_else(|| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} declared but missing in SPIR-V",
                descriptor.label, spec.binding
            ))
        })?;
        if *spirv_kind != spec.kind {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} declared {:?}, but SPIR-V has {:?}",
                descriptor.label, spec.binding, spec.kind, spirv_kind
            )));
        }
        if !spec.stages.contains(*spirv_stages) {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} stages {:?} miss SPIR-V usage in {:?}",
                descriptor.label, spec.binding, spec.stages, spirv_stages
            )));
        }
    }

    // Conversely, every SPIR-V binding must be declared.
    for (&binding, &(kind, _)) in &merged {
        if !descriptor.bindings.iter().any(|s| s.binding == binding) {
            return Err(StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': SPIR-V declares binding {} ({:?}) but it is missing from the descriptor",
                descriptor.label, binding, kind
            )));
        }
    }

    Ok(())
}

fn validate_push_constants_against_spirv(
    descriptor: &RayTracingKernelDescriptor<'_>,
) -> Result<()> {
    let mut max_size = 0u32;
    for stage in descriptor.stages {
        let reflection = Reflection::new_from_spirv(stage.spv).map_err(|e| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': failed to reflect SPIR-V (push) for stage {:?}: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        if let Some(info) = reflection.get_push_constant_range().map_err(|e| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': failed to read push-constant range for stage {:?}: {e:?}",
                descriptor.label, stage.stage
            ))
        })? {
            max_size = max_size.max(info.size);
        }
    }
    if max_size != descriptor.push_constants.size {
        return Err(StreamError::GpuError(format!(
            "Ray-tracing kernel '{}': push-constant size mismatch — SPIR-V says {}, descriptor declares {}",
            descriptor.label, max_size, descriptor.push_constants.size
        )));
    }
    Ok(())
}

fn spirv_type_to_kind(ty: RDescriptorType) -> Option<RayTracingBindingKind> {
    match ty {
        RDescriptorType::STORAGE_BUFFER => Some(RayTracingBindingKind::StorageBuffer),
        RDescriptorType::UNIFORM_BUFFER => Some(RayTracingBindingKind::UniformBuffer),
        RDescriptorType::COMBINED_IMAGE_SAMPLER => {
            Some(RayTracingBindingKind::SampledTexture)
        }
        RDescriptorType::STORAGE_IMAGE => Some(RayTracingBindingKind::StorageImage),
        RDescriptorType::ACCELERATION_STRUCTURE_KHR => {
            Some(RayTracingBindingKind::AccelerationStructure)
        }
        _ => None,
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
        .map_err(|e| StreamError::GpuError(format!("RT descriptor-set-layout creation failed: {e}")))
}

fn create_pipeline_layout(
    device: &vulkanalia::Device,
    set_layout: vk::DescriptorSetLayout,
    push_constant_size: u32,
    push_stages: RayTracingShaderStageFlags,
) -> Result<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push_constant_ranges: Vec<vk::PushConstantRange> = if push_constant_size > 0 {
        vec![vk::PushConstantRange::builder()
            .stage_flags(stage_flags_to_vk(push_stages))
            .offset(0)
            .size(push_constant_size)
            .build()]
    } else {
        Vec::new()
    };
    let info = vk::PipelineLayoutCreateInfo::builder()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constant_ranges)
        .build();
    unsafe { device.create_pipeline_layout(&info, None) }
        .map_err(|e| StreamError::GpuError(format!("RT pipeline-layout creation failed: {e}")))
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
        .map_err(|e| StreamError::GpuError(format!("RT descriptor-pool creation failed: {e}")))
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
        .map_err(|e| StreamError::GpuError(format!("RT descriptor-set allocation failed: {e}")))?;
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
        .map_err(|e| StreamError::GpuError(format!("RT command-pool creation failed: {e}")))
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
        .map_err(|e| StreamError::GpuError(format!("RT command-buffer allocation failed: {e}")))?;
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

/// Categorize each group into one of the four SBT regions.
#[derive(Clone, Copy)]
enum SbtRegion {
    RayGen,
    Miss,
    Hit,
    Callable,
}

fn group_region(
    stages: &[RayTracingStage<'_>],
    group: &RayTracingShaderGroup,
) -> SbtRegion {
    match *group {
        RayTracingShaderGroup::General { general } => match stages[general as usize].stage {
            RayTracingShaderStage::RayGen => SbtRegion::RayGen,
            RayTracingShaderStage::Miss => SbtRegion::Miss,
            RayTracingShaderStage::Callable => SbtRegion::Callable,
            other => unreachable!(
                "validate_shader_groups must reject non-{{RayGen,Miss,Callable}} stages in a General group, got {:?}",
                other
            ),
        },
        RayTracingShaderGroup::TrianglesHit { .. }
        | RayTracingShaderGroup::ProceduralHit { .. } => SbtRegion::Hit,
    }
}

fn align_up(value: vk::DeviceSize, alignment: vk::DeviceSize) -> vk::DeviceSize {
    if alignment == 0 {
        value
    } else {
        (value + alignment - 1) & !(alignment - 1)
    }
}

fn build_sbt(
    vulkan_device: &Arc<HostVulkanDevice>,
    descriptor: &RayTracingKernelDescriptor<'_>,
    pipeline: vk::Pipeline,
    rt_props: &super::RayTracingPipelineProperties,
) -> Result<Sbt> {
    let device = vulkan_device.device();

    let handle_size = rt_props.shader_group_handle_size as vk::DeviceSize;
    let handle_stride =
        align_up(handle_size, rt_props.shader_group_handle_alignment as vk::DeviceSize);
    let base_alignment = rt_props.shader_group_base_alignment as vk::DeviceSize;

    // Categorize each group. The vkGetRayTracingShaderGroupHandlesKHR call
    // returns handles in the same order as the groups in the pipeline-create
    // info, so `group_indices` matches descriptor.groups index-for-index.
    let mut raygen_indices = Vec::new();
    let mut miss_indices = Vec::new();
    let mut hit_indices = Vec::new();
    let mut callable_indices = Vec::new();
    for (i, g) in descriptor.groups.iter().enumerate() {
        match group_region(descriptor.stages, g) {
            SbtRegion::RayGen => raygen_indices.push(i),
            SbtRegion::Miss => miss_indices.push(i),
            SbtRegion::Hit => hit_indices.push(i),
            SbtRegion::Callable => callable_indices.push(i),
        }
    }
    if raygen_indices.is_empty() {
        return Err(StreamError::GpuError(format!(
            "Ray-tracing kernel '{}': pipeline must have at least one RayGen group",
            descriptor.label
        )));
    }

    // Region offsets (within the SBT buffer) are aligned to base_alignment.
    let raygen_offset: vk::DeviceSize = 0;
    let raygen_size =
        align_up(handle_stride * raygen_indices.len() as vk::DeviceSize, base_alignment);

    let miss_offset = raygen_offset + raygen_size;
    let miss_size =
        align_up(handle_stride * miss_indices.len() as vk::DeviceSize, base_alignment);

    let hit_offset = miss_offset + miss_size;
    let hit_size =
        align_up(handle_stride * hit_indices.len() as vk::DeviceSize, base_alignment);

    let callable_offset = hit_offset + hit_size;
    let callable_size = align_up(
        handle_stride * callable_indices.len() as vk::DeviceSize,
        base_alignment,
    );

    let total_size = (callable_offset + callable_size).max(handle_stride);

    // Fetch all group handles into a single buffer.
    let group_count = descriptor.groups.len() as u32;
    let handle_data_size = (handle_size * group_count as vk::DeviceSize) as usize;
    let mut handle_blob = vec![0u8; handle_data_size];
    unsafe {
        device.get_ray_tracing_shader_group_handles_khr(
            pipeline,
            0,
            group_count,
            &mut handle_blob,
        )
    }
    .map_err(|e| {
        StreamError::GpuError(format!(
            "Ray-tracing kernel '{}': vkGetRayTracingShaderGroupHandlesKHR failed: {e}",
            descriptor.label
        ))
    })?;

    // Build the SBT in a CPU staging buffer, then upload to a DEVICE_LOCAL
    // SBT buffer via vkCmdCopyBuffer. Direct DEVICE_LOCAL mapping isn't
    // portable across drivers (NVIDIA-style discrete VRAM is not
    // host-visible), so the staging path is the conservative shape.
    let mut staging_data = vec![0u8; total_size as usize];
    write_region(
        &mut staging_data,
        raygen_offset,
        handle_stride,
        &raygen_indices,
        &handle_blob,
        handle_size,
    );
    write_region(
        &mut staging_data,
        miss_offset,
        handle_stride,
        &miss_indices,
        &handle_blob,
        handle_size,
    );
    write_region(
        &mut staging_data,
        hit_offset,
        handle_stride,
        &hit_indices,
        &handle_blob,
        handle_size,
    );
    write_region(
        &mut staging_data,
        callable_offset,
        handle_stride,
        &callable_indices,
        &handle_blob,
        handle_size,
    );

    // Allocate the DEVICE_LOCAL SBT buffer.
    let allocator = vulkan_device.allocator();
    let buffer_info = vk::BufferCreateInfo::builder()
        .size(total_size)
        .usage(
            vk::BufferUsageFlags::SHADER_BINDING_TABLE_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                | vk::BufferUsageFlags::TRANSFER_DST,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .build();
    let alloc_opts = vma::AllocationOptions {
        usage: vma::MemoryUsage::AutoPreferDevice,
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };
    let (sbt_buffer, sbt_allocation) =
        unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }.map_err(|e| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{}': SBT vmaCreateBuffer (size={total_size}) failed: {e}",
                descriptor.label
            ))
        })?;
    let address_info = vk::BufferDeviceAddressInfo::builder().buffer(sbt_buffer).build();
    let sbt_address = unsafe { device.get_buffer_device_address(&address_info) };

    // Stage + upload.
    if let Err(e) = upload_bytes_to_buffer(
        vulkan_device,
        sbt_buffer,
        &staging_data,
        descriptor.label,
    ) {
        unsafe { allocator.destroy_buffer(sbt_buffer, sbt_allocation) };
        return Err(e);
    }

    Ok(Sbt {
        buffer: sbt_buffer,
        allocation: Some(sbt_allocation),
        raygen_region: vk::StridedDeviceAddressRegionKHR {
            device_address: sbt_address + raygen_offset,
            stride: handle_stride,
            size: handle_stride * raygen_indices.len() as vk::DeviceSize,
        },
        miss_region: vk::StridedDeviceAddressRegionKHR {
            device_address: if miss_indices.is_empty() {
                0
            } else {
                sbt_address + miss_offset
            },
            stride: if miss_indices.is_empty() { 0 } else { handle_stride },
            size: handle_stride * miss_indices.len() as vk::DeviceSize,
        },
        hit_region: vk::StridedDeviceAddressRegionKHR {
            device_address: if hit_indices.is_empty() {
                0
            } else {
                sbt_address + hit_offset
            },
            stride: if hit_indices.is_empty() { 0 } else { handle_stride },
            size: handle_stride * hit_indices.len() as vk::DeviceSize,
        },
        callable_region: vk::StridedDeviceAddressRegionKHR {
            device_address: if callable_indices.is_empty() {
                0
            } else {
                sbt_address + callable_offset
            },
            stride: if callable_indices.is_empty() { 0 } else { handle_stride },
            size: handle_stride * callable_indices.len() as vk::DeviceSize,
        },
    })
}

fn write_region(
    out: &mut [u8],
    region_offset: vk::DeviceSize,
    stride: vk::DeviceSize,
    group_indices: &[usize],
    handle_blob: &[u8],
    handle_size: vk::DeviceSize,
) {
    for (i, &group_idx) in group_indices.iter().enumerate() {
        let dst_off = (region_offset + stride * i as vk::DeviceSize) as usize;
        let src_off = group_idx * handle_size as usize;
        let len = handle_size as usize;
        out[dst_off..dst_off + len]
            .copy_from_slice(&handle_blob[src_off..src_off + len]);
    }
}

fn upload_bytes_to_buffer(
    vulkan_device: &Arc<HostVulkanDevice>,
    dst_buffer: vk::Buffer,
    bytes: &[u8],
    label: &str,
) -> Result<()> {
    let device = vulkan_device.device();
    let allocator = vulkan_device.allocator();
    let size = bytes.len() as vk::DeviceSize;
    if size == 0 {
        return Ok(());
    }

    let staging_info = vk::BufferCreateInfo::builder()
        .size(size)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .build();
    let staging_opts = vma::AllocationOptions {
        usage: vma::MemoryUsage::AutoPreferHost,
        required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        flags: vma::AllocationCreateFlags::MAPPED
            | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
        ..Default::default()
    };
    let (staging_buffer, staging_allocation) =
        unsafe { allocator.create_buffer(staging_info, &staging_opts) }.map_err(|e| {
            StreamError::GpuError(format!(
                "Ray-tracing kernel '{label}': SBT staging vmaCreateBuffer ({size}) failed: {e}"
            ))
        })?;
    let info = allocator.get_allocation_info(staging_allocation);
    let dst_ptr = info.pMappedData as *mut u8;
    if dst_ptr.is_null() {
        unsafe { allocator.destroy_buffer(staging_buffer, staging_allocation) };
        return Err(StreamError::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT staging mapping returned null"
        )));
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst_ptr, bytes.len()) };

    let queue = vulkan_device.queue();
    let queue_family = vulkan_device.queue_family_index();
    let command_pool = create_command_pool(device, queue_family).map_err(|e| {
        unsafe { allocator.destroy_buffer(staging_buffer, staging_allocation) };
        e
    })?;
    let cmd = match allocate_command_buffer(device, command_pool) {
        Ok(c) => c,
        Err(e) => {
            unsafe {
                device.destroy_command_pool(command_pool, None);
                allocator.destroy_buffer(staging_buffer, staging_allocation);
            }
            return Err(e);
        }
    };
    let begin = vk::CommandBufferBeginInfo::builder()
        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
        .build();
    unsafe { device.begin_command_buffer(cmd, &begin) }.map_err(|e| {
        unsafe {
            device.destroy_command_pool(command_pool, None);
            allocator.destroy_buffer(staging_buffer, staging_allocation);
        }
        StreamError::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT begin_command_buffer failed: {e}"
        ))
    })?;
    let region = vk::BufferCopy::builder()
        .src_offset(0)
        .dst_offset(0)
        .size(size)
        .build();
    unsafe { device.cmd_copy_buffer(cmd, staging_buffer, dst_buffer, &[region]) };
    unsafe { device.end_command_buffer(cmd) }.map_err(|e| {
        unsafe {
            device.destroy_command_pool(command_pool, None);
            allocator.destroy_buffer(staging_buffer, staging_allocation);
        }
        StreamError::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT end_command_buffer failed: {e}"
        ))
    })?;

    let fence_info = vk::FenceCreateInfo::builder().build();
    let fence = unsafe { device.create_fence(&fence_info, None) }.map_err(|e| {
        unsafe {
            device.destroy_command_pool(command_pool, None);
            allocator.destroy_buffer(staging_buffer, staging_allocation);
        }
        StreamError::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT fence creation failed: {e}"
        ))
    })?;

    let cmd_info = vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build();
    let cmd_infos = [cmd_info];
    let submit = vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build();

    let submit_result = unsafe {
        HostVulkanDevice::submit_to_queue(vulkan_device, queue, &[submit], fence)
    };
    let wait_result: Result<()> = if submit_result.is_ok() {
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map(|_| ())
            .map_err(|e| {
                StreamError::GpuError(format!(
                    "Ray-tracing kernel '{label}': SBT wait_for_fences failed: {e}"
                ))
            })
    } else {
        Ok(())
    };

    unsafe {
        device.destroy_fence(fence, None);
        device.destroy_command_pool(command_pool, None);
        allocator.destroy_buffer(staging_buffer, staging_allocation);
    }

    submit_result.and(wait_result)
}

fn drop_sbt(sbt: &Sbt, vulkan_device: &Arc<HostVulkanDevice>) {
    if let Some(allocation) = sbt.allocation.as_ref().copied() {
        unsafe { vulkan_device.allocator().destroy_buffer(sbt.buffer, allocation) };
    }
}

fn vk_buffer_for(buffer: &RhiPixelBuffer) -> (vk::Buffer, vk::DeviceSize) {
    let inner = &buffer.buffer_ref().inner;
    (inner.buffer(), inner.size())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{
        RayTracingBindingSpec, RayTracingKernelDescriptor, RayTracingPushConstants,
        RayTracingShaderGroup, RayTracingShaderStageFlags, RayTracingStage, StreamTexture,
        TextureDescriptor, TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
        TextureUsages,
    };
    use crate::vulkan::rhi::{
        HostVulkanDevice, HostVulkanTexture, TlasInstanceDesc, VulkanAccelerationStructure,
        VulkanTextureReadback,
    };

    /// Smoke check both that a Vulkan device is available AND that it
    /// exposes the `VK_KHR_ray_tracing_pipeline` extension chain. Tests
    /// that need RT skip cleanly when either is missing — no devices are
    /// faked.
    fn try_ray_tracing_device() -> Option<Arc<HostVulkanDevice>> {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return None;
            }
        };
        if !device.supports_ray_tracing_pipeline() {
            println!("Skipping - device does not expose VK_KHR_ray_tracing_pipeline");
            return None;
        }
        Some(device)
    }

    fn rt_test_rgen_spv() -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/raytracing_test.rgen.spv"))
    }
    fn rt_test_rmiss_spv() -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/raytracing_test.rmiss.spv"))
    }
    fn rt_test_rchit_spv() -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/raytracing_test.rchit.spv"))
    }

    fn make_test_kernel(
        device: &Arc<HostVulkanDevice>,
        label: &str,
    ) -> Result<VulkanRayTracingKernel> {
        let stages = [
            RayTracingStage::ray_gen(rt_test_rgen_spv()),
            RayTracingStage::miss(rt_test_rmiss_spv()),
            RayTracingStage::closest_hit(rt_test_rchit_spv()),
        ];
        let groups = [
            RayTracingShaderGroup::General { general: 0 },
            RayTracingShaderGroup::General { general: 1 },
            RayTracingShaderGroup::TrianglesHit {
                closest_hit: Some(2),
                any_hit: None,
            },
        ];
        let bindings = [
            RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
            RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
        ];
        VulkanRayTracingKernel::new(
            device,
            &RayTracingKernelDescriptor {
                label,
                stages: &stages,
                groups: &groups,
                bindings: &bindings,
                push_constants: RayTracingPushConstants::NONE,
                max_recursion_depth: 1,
            },
        )
    }

    #[test]
    fn kernel_constructs_against_real_device() {
        let Some(device) = try_ray_tracing_device() else { return };
        let _kernel = make_test_kernel(&device, "rt-construct").expect("kernel creation");
    }

    #[test]
    fn kernel_rejects_missing_binding_in_descriptor() {
        let Some(device) = try_ray_tracing_device() else { return };
        // Shader binds 0=AS + 1=storage image; omit binding 1.
        let stages = [
            RayTracingStage::ray_gen(rt_test_rgen_spv()),
            RayTracingStage::miss(rt_test_rmiss_spv()),
            RayTracingStage::closest_hit(rt_test_rchit_spv()),
        ];
        let groups = [
            RayTracingShaderGroup::General { general: 0 },
            RayTracingShaderGroup::General { general: 1 },
            RayTracingShaderGroup::TrianglesHit {
                closest_hit: Some(2),
                any_hit: None,
            },
        ];
        let bindings = [RayTracingBindingSpec::acceleration_structure(
            0,
            RayTracingShaderStageFlags::RAYGEN,
        )];
        let result = VulkanRayTracingKernel::new(
            &device,
            &RayTracingKernelDescriptor {
                label: "rt-missing-binding",
                stages: &stages,
                groups: &groups,
                bindings: &bindings,
                push_constants: RayTracingPushConstants::NONE,
                max_recursion_depth: 1,
            },
        );
        let err = result.err().expect("expected validation failure");
        assert!(
            format!("{err}").contains("binding 1"),
            "expected error about missing binding 1, got: {err}"
        );
    }

    #[test]
    fn kernel_rejects_kind_mismatch() {
        let Some(device) = try_ray_tracing_device() else { return };
        let stages = [
            RayTracingStage::ray_gen(rt_test_rgen_spv()),
            RayTracingStage::miss(rt_test_rmiss_spv()),
            RayTracingStage::closest_hit(rt_test_rchit_spv()),
        ];
        let groups = [
            RayTracingShaderGroup::General { general: 0 },
            RayTracingShaderGroup::General { general: 1 },
            RayTracingShaderGroup::TrianglesHit {
                closest_hit: Some(2),
                any_hit: None,
            },
        ];
        // Declare binding 0 (which the shader has as AS) as a storage buffer.
        let bindings = [
            RayTracingBindingSpec::storage_buffer(0, RayTracingShaderStageFlags::RAYGEN),
            RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
        ];
        let result = VulkanRayTracingKernel::new(
            &device,
            &RayTracingKernelDescriptor {
                label: "rt-kind-mismatch",
                stages: &stages,
                groups: &groups,
                bindings: &bindings,
                push_constants: RayTracingPushConstants::NONE,
                max_recursion_depth: 1,
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 0") && msg.contains("StorageBuffer"),
            "expected mismatch error mentioning binding 0 + StorageBuffer, got: {msg}"
        );
    }

    /// Build a 1-triangle BLAS, single-instance TLAS, and run trace-rays
    /// against a 64×64 storage image. Reads the result back and checks
    /// that the centre pixel is hit (barycentric color, mostly red) and
    /// the corner pixels are miss (dark blue from rmiss).
    #[test]
    fn trace_rays_produces_hit_and_miss_pixels() {
        let Some(device) = try_ray_tracing_device() else { return };

        // Triangle in clip-space-ish coords: covers the centre of the
        // [-1,1]² launch window, leaves the corners uncovered.
        let vertices: [f32; 9] = [
            -0.6, -0.6, 0.5, //
             0.6, -0.6, 0.5, //
             0.0,  0.6, 0.5, //
        ];
        let indices: [u32; 3] = [0, 1, 2];
        let blas = VulkanAccelerationStructure::build_triangles_blas(
            &device,
            "rt-test-blas",
            &vertices,
            &indices,
        )
        .expect("BLAS build");
        assert_eq!(
            blas.kind(),
            crate::vulkan::rhi::AccelerationStructureKind::BottomLevel
        );
        assert!(
            blas.device_address() != 0,
            "BLAS device_address must be non-zero — got {:#x}",
            blas.device_address()
        );

        let instance = TlasInstanceDesc::identity(Arc::clone(&blas));
        let tlas = VulkanAccelerationStructure::build_tlas(
            &device,
            "rt-test-tlas",
            std::slice::from_ref(&instance),
        )
        .expect("TLAS build");
        assert!(
            tlas.device_address() != 0,
            "TLAS device_address must be non-zero"
        );

        // Storage image (64×64 RGBA8) + bring it into GENERAL before the kernel binds.
        const W: u32 = 64;
        const H: u32 = 64;
        let texture = HostVulkanTexture::new_device_local(
            &device,
            &TextureDescriptor {
                label: Some("rt-test-output"),
                width: W,
                height: H,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
            },
        )
        .expect("texture creation");
        let stream_texture = StreamTexture::from_vulkan(texture);
        let image = stream_texture
            .vulkan_inner()
            .image()
            .expect("image handle");
        HostVulkanTexture::transition_to_general(&device, image)
            .expect("transition output to GENERAL");

        let kernel = make_test_kernel(&device, "rt-trace-test").expect("kernel creation");
        kernel
            .set_acceleration_structure(0, &tlas)
            .expect("set TLAS");
        kernel
            .set_storage_image(1, &stream_texture)
            .expect("set storage image");
        kernel
            .trace_rays(W, H, 1)
            .expect("trace_rays");

        let readback = VulkanTextureReadback::new_into_stream_error(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-test-readback",
                format: TextureFormat::Rgba8Unorm,
                width: W,
                height: H,
            },
        )
        .expect("readback construction");
        let ticket = readback
            .submit(&stream_texture, TextureSourceLayout::General)
            .expect("readback submit");
        let bytes = readback
            .wait_and_read(ticket, u64::MAX)
            .expect("readback wait_and_read");

        // Centre pixel — closest-hit shader writes barycentric color.
        // Triangle vertices arranged so the centre ray hits at barycentric
        // ≈ (0, 1/3, 2/3), which the rchit shader stores as
        // (red=0, green~85, blue~170) in RGBA8. The miss shader writes
        // a dark blue (13, 13, 51) — green is the discriminator: above
        // 50 means we hit, below 30 means we missed. The test was
        // originally too lenient (`any channel > 0`) — that passed even
        // when *every* ray missed (sky color is non-zero), so the test
        // didn't lock in the AS-actually-being-hit invariant.
        let centre_idx = ((H / 2) * W + (W / 2)) as usize * 4;
        let centre_g = bytes[centre_idx + 1];
        assert!(
            centre_g > 50,
            "centre pixel should be lit by closest-hit shader (green > 50 from barycentric blend), got rgb=({}, {}, {}). Likely the AS isn't being hit — every ray returned the miss-shader color.",
            bytes[centre_idx],
            bytes[centre_idx + 1],
            bytes[centre_idx + 2],
        );

        // Corner pixel — should be miss-shader's dark blue (~ 0.05, 0.05, 0.20).
        let corner = (bytes[0], bytes[1], bytes[2]);
        assert!(
            corner.2 > corner.0 && corner.2 > corner.1 && corner.2 < 100,
            "corner pixel should be dark blue from miss shader (~13,13,51); got rgb=({}, {}, {})",
            corner.0,
            corner.1,
            corner.2
        );
    }

}
