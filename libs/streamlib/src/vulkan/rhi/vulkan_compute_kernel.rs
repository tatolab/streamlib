// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan compute-kernel RHI: shader pipeline + descriptor set + dispatch primitives.
//!
//! Pattern (Granite-style): the kernel author declares the binding layout once
//! as data; the RHI reflects the SPIR-V at creation, validates the declaration
//! against the shader, and from that point on the user calls typed setters by
//! slot. `dispatch()` flushes pending descriptor writes, records the dispatch
//! command buffer, submits, and waits.
//!
//! Binding kinds supported today: storage buffer, uniform buffer, sampled
//! texture (with a default linear-clamp sampler), storage image. All bindings
//! live on descriptor set 0.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};

use crate::core::rhi::{
    ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor, RhiPixelBuffer, StreamTexture,
};
use crate::core::{Result, StreamError};

/// Env var that overrides the default pipeline-cache directory. Used by tests
/// and headless / CI scenarios that need a writable, isolated cache root.
pub const PIPELINE_CACHE_DIR_ENV: &str = "STREAMLIB_PIPELINE_CACHE_DIR";

use super::HostVulkanDevice;

/// One compute kernel: shader pipeline + descriptor set + per-dispatch primitives.
///
/// `set_*` methods stage descriptor writes; `dispatch(x, y, z)` flushes them,
/// records the command buffer, submits, and waits for completion. Each kernel
/// holds a single descriptor set, so dispatches against it are serial — that's
/// fine for the format-converter / compositor / codec-pre-process workloads
/// this abstraction targets.
pub struct VulkanComputeKernel {
    label: String,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    bindings: Vec<ComputeBindingSpec>,
    push_constant_size: u32,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    shader_module: vk::ShaderModule,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// Default sampler used for [`ComputeBindingKind::SampledTexture`] bindings.
    /// Created on-demand when the first sampled-texture binding is set.
    default_sampler: Mutex<Option<vk::Sampler>>,
    pending: Mutex<PendingState>,
}

struct PendingState {
    bindings: HashMap<u32, BindingResource>,
    push_constants: Vec<u8>,
}

#[derive(Clone, Copy)]
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
}

impl VulkanComputeKernel {
    /// Create a new compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Reflects the SPIR-V via `rspirv-reflect`, validates that the declared
    /// `bindings` match the shader's descriptor types, and rejects any mismatch
    /// before allocating Vulkan objects.
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
    ) -> Result<Self> {
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();
        let device = vulkan_device.device();

        validate_against_spirv(descriptor)?;

        let spirv: Vec<u32> = descriptor
            .spv
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        // ---- Vulkan objects ----------------------------------------------------------
        // Created in a strict order; on failure earlier objects are torn down by the
        // staged-cleanup helpers so we never leak on the error path.

        let shader_module = create_shader_module(device, &spirv, descriptor.label)?;

        let descriptor_set_layout = match create_descriptor_set_layout(device, descriptor.bindings)
        {
            Ok(layout) => layout,
            Err(e) => {
                unsafe { device.destroy_shader_module(shader_module, None) };
                return Err(e);
            }
        };

        let pipeline_layout = match create_pipeline_layout(
            device,
            descriptor_set_layout,
            descriptor.push_constant_size,
        ) {
            Ok(layout) => layout,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let pipeline = match create_compute_pipeline_with_cache(
            device,
            shader_module,
            pipeline_layout,
            descriptor.spv,
            descriptor.label,
        ) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let descriptor_pool = match create_descriptor_pool(device, descriptor.bindings) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
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
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let command_pool = match create_command_pool(device, queue_family_index) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let command_buffer = match allocate_command_buffer(device, command_pool) {
            Ok(cb) => cb,
            Err(e) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
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
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(StreamError::GpuError(format!(
                    "Failed to create compute fence: {e}"
                )));
            }
        };

        Ok(Self {
            label: descriptor.label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            device: device.clone(),
            queue,
            bindings: descriptor.bindings.to_vec(),
            push_constant_size: descriptor.push_constant_size,
            pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            shader_module,
            command_pool,
            command_buffer,
            fence,
            default_sampler: Mutex::new(None),
            pending: Mutex::new(PendingState {
                bindings: HashMap::new(),
                push_constants: Vec::new(),
            }),
        })
    }

    /// Bind a storage buffer at `binding`. The slot must be declared as
    /// [`ComputeBindingKind::StorageBuffer`] in the descriptor.
    pub fn set_storage_buffer(&self, binding: u32, buffer: &RhiPixelBuffer) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::StorageBuffer)?;
        let (vk_buf, size) = vk_buffer_for(buffer)?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: vk_buf,
                size,
            },
        );
        Ok(())
    }

    /// Bind a uniform buffer at `binding`. The slot must be declared as
    /// [`ComputeBindingKind::UniformBuffer`] in the descriptor.
    pub fn set_uniform_buffer(&self, binding: u32, buffer: &RhiPixelBuffer) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::UniformBuffer)?;
        let (vk_buf, size) = vk_buffer_for(buffer)?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: vk_buf,
                size,
            },
        );
        Ok(())
    }

    /// Bind a sampled texture at `binding`, using the kernel's default
    /// linear-clamp sampler.
    pub fn set_sampled_texture(&self, binding: u32, texture: &StreamTexture) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::SampledTexture)?;
        let view = vk_image_view_for(texture)?;
        let sampler = self.default_sampler()?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::SampledImage { view, sampler },
        );
        Ok(())
    }

    /// Bind a storage image at `binding`. Caller is responsible for ensuring
    /// the texture's `STORAGE_BINDING` usage was set when it was created.
    pub fn set_storage_image(&self, binding: u32, texture: &StreamTexture) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::StorageImage)?;
        let view = vk_image_view_for(texture)?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::StorageImage { view });
        Ok(())
    }

    /// Stage push constants for the next dispatch. Size must match the kernel's
    /// declared `push_constant_size` (and be 4-byte aligned per Vulkan).
    pub fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u32 != self.push_constant_size {
            return Err(StreamError::GpuError(format!(
                "Compute kernel '{}': push-constant size mismatch — got {} bytes, kernel declares {}",
                self.label,
                bytes.len(),
                self.push_constant_size
            )));
        }
        self.pending.lock().push_constants = bytes.to_vec();
        Ok(())
    }

    /// Convenience: stage a `Copy` value as push constants by reinterpreting its
    /// bytes. The value's size in bytes must match `push_constant_size`.
    pub fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let bytes =
            unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size) };
        self.set_push_constants(bytes)
    }

    /// Run the kernel: write all staged descriptors, record bind+push+dispatch,
    /// submit to the device queue, and wait on the kernel fence before returning.
    ///
    /// Every binding declared at construction must have been set since the last
    /// dispatch (unset bindings are an error — Vulkan's behavior in that case
    /// is undefined, so we refuse to dispatch).
    pub fn dispatch(&self, group_count_x: u32, group_count_y: u32, group_count_z: u32) -> Result<()> {
        // Drain pending state up-front so concurrent set_* calls during the
        // dispatch do not leak into the next one.
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
                    "Compute kernel '{}': binding {} ({:?}) not set before dispatch",
                    self.label, spec.binding, spec.kind
                )));
            }
        }
        if self.push_constant_size > 0 && pending.push_constants.is_empty() {
            return Err(StreamError::GpuError(format!(
                "Compute kernel '{}': push constants not set before dispatch",
                self.label
            )));
        }

        // Wait for prior dispatch (if any) to drain so the command buffer is
        // safe to reset.
        unsafe {
            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to wait for compute fence: {e}"))
                })?;
            self.device
                .reset_fences(&[self.fence])
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset compute fence: {e}"))
                })?;
        }

        self.flush_descriptor_writes(&pending)?;

        unsafe {
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset command buffer: {e}"))
                })?;

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();

            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to begin command buffer: {e}"))
                })?;

            self.device.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );

            self.device.cmd_bind_descriptor_sets(
                self.command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );

            if self.push_constant_size > 0 {
                self.device.cmd_push_constants(
                    self.command_buffer,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    &pending.push_constants,
                );
            }

            self.device.cmd_dispatch(
                self.command_buffer,
                group_count_x,
                group_count_y,
                group_count_z,
            );

            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to end command buffer: {e}"))
                })?;

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();

            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], self.fence)?;

            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to wait for compute fence: {e}"))
                })?;
        }

        Ok(())
    }

    /// Bindings declared at construction time, in declaration order.
    pub fn bindings(&self) -> &[ComputeBindingSpec] {
        &self.bindings
    }

    /// Push-constant size in bytes (0 if the kernel has none).
    pub fn push_constant_size(&self) -> u32 {
        self.push_constant_size
    }

    fn expect_kind(&self, binding: u32, expected: ComputeBindingKind) -> Result<()> {
        let spec = self.bindings.iter().find(|b| b.binding == binding).ok_or_else(|| {
            StreamError::GpuError(format!(
                "Compute kernel '{}': binding {} not declared",
                self.label, binding
            ))
        })?;
        if spec.kind != expected {
            return Err(StreamError::GpuError(format!(
                "Compute kernel '{}': binding {} declared as {:?}, but {:?} was set",
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
        let sampler = unsafe { self.device.create_sampler(&info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create default sampler: {e}")))?;
        *guard = Some(sampler);
        Ok(sampler)
    }

    fn flush_descriptor_writes(&self, pending: &PendingState) -> Result<()> {
        let mut buffer_infos: Vec<vk::DescriptorBufferInfo> = Vec::with_capacity(self.bindings.len());
        let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::with_capacity(self.bindings.len());
        let mut writes: Vec<vk::WriteDescriptorSet> = Vec::with_capacity(self.bindings.len());

        // Build the *_info vectors first so all slot pointers are stable, then
        // build writes that reference them by index.
        struct Slot {
            binding: u32,
            ty: vk::DescriptorType,
            buffer_idx: Option<usize>,
            image_idx: Option<usize>,
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(self.bindings.len());

        for spec in &self.bindings {
            let res = pending.bindings.get(&spec.binding).expect("checked above");
            match (spec.kind, res) {
                (ComputeBindingKind::StorageBuffer, BindingResource::Buffer { buffer, size }) => {
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
                    });
                }
                (ComputeBindingKind::UniformBuffer, BindingResource::Buffer { buffer, size }) => {
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
                    });
                }
                (
                    ComputeBindingKind::SampledTexture,
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
                    });
                }
                (ComputeBindingKind::StorageImage, BindingResource::StorageImage { view }) => {
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
                    });
                }
                _ => {
                    return Err(StreamError::GpuError(format!(
                        "Compute kernel '{}': binding {} kind/resource mismatch (declared {:?})",
                        self.label, spec.binding, spec.kind
                    )));
                }
            }
        }

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
            writes.push(write.build());
        }

        unsafe {
            self.device
                .update_descriptor_sets(&writes, &[] as &[vk::CopyDescriptorSet]);
        }
        Ok(())
    }
}

impl Drop for VulkanComputeKernel {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
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
            self.device.destroy_shader_module(self.shader_module, None);
        }
    }
}

// Vulkan handles in this struct are protected by the kernel's owned fence:
// only one dispatch is in-flight at a time, and `dispatch()` blocks until
// the GPU completes. The `Mutex` around `pending` serializes setter writes
// across threads.
unsafe impl Send for VulkanComputeKernel {}
unsafe impl Sync for VulkanComputeKernel {}

impl std::fmt::Debug for VulkanComputeKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanComputeKernel")
            .field("label", &self.label)
            .field("bindings", &self.bindings)
            .field("push_constant_size", &self.push_constant_size)
            .finish()
    }
}

// ---- Validation + creation helpers --------------------------------------------

fn validate_against_spirv(descriptor: &ComputeKernelDescriptor<'_>) -> Result<()> {
    let reflection = Reflection::new_from_spirv(descriptor.spv).map_err(|e| {
        StreamError::GpuError(format!(
            "Compute kernel '{}': failed to reflect SPIR-V: {e:?}",
            descriptor.label
        ))
    })?;

    let sets = reflection.get_descriptor_sets().map_err(|e| {
        StreamError::GpuError(format!(
            "Compute kernel '{}': failed to extract descriptor sets: {e:?}",
            descriptor.label
        ))
    })?;

    // Reject multi-set kernels — out of scope.
    if sets.len() > 1 {
        return Err(StreamError::GpuError(format!(
            "Compute kernel '{}': only descriptor set 0 is supported; SPIR-V uses sets {:?}",
            descriptor.label,
            sets.keys().collect::<Vec<_>>()
        )));
    }

    let set0 = sets.get(&0);

    // For each declared binding, the SPIR-V must agree on (a) presence and (b) descriptor type.
    for spec in descriptor.bindings {
        let info = set0
            .and_then(|m| m.get(&spec.binding))
            .ok_or_else(|| {
                StreamError::GpuError(format!(
                    "Compute kernel '{}': binding {} declared but missing in SPIR-V",
                    descriptor.label, spec.binding
                ))
            })?;
        let expected = expected_spirv_type(spec.kind);
        if info.ty != expected {
            return Err(StreamError::GpuError(format!(
                "Compute kernel '{}': binding {} declared {:?} ({:?}), but SPIR-V has {:?}",
                descriptor.label, spec.binding, spec.kind, expected, info.ty
            )));
        }
    }

    // Conversely, every binding the SPIR-V declares must be present in the
    // declaration — silently leaving a shader binding unset is undefined behavior.
    if let Some(set0) = set0 {
        for (&binding, info) in set0 {
            if !descriptor.bindings.iter().any(|s| s.binding == binding) {
                return Err(StreamError::GpuError(format!(
                    "Compute kernel '{}': SPIR-V declares binding {} ({:?}, name `{}`) but it is missing from the descriptor",
                    descriptor.label, binding, info.ty, info.name
                )));
            }
        }
    }

    // Push-constant size must match if the shader uses any.
    let push = reflection.get_push_constant_range().map_err(|e| {
        StreamError::GpuError(format!(
            "Compute kernel '{}': failed to read push-constant range: {e:?}",
            descriptor.label
        ))
    })?;
    match (push, descriptor.push_constant_size) {
        (Some(info), declared) if info.size != declared => {
            return Err(StreamError::GpuError(format!(
                "Compute kernel '{}': push-constant size mismatch — SPIR-V says {}, descriptor declares {}",
                descriptor.label, info.size, declared
            )));
        }
        (None, declared) if declared > 0 => {
            return Err(StreamError::GpuError(format!(
                "Compute kernel '{}': descriptor declares {} push-constant bytes but SPIR-V has none",
                descriptor.label, declared
            )));
        }
        _ => {}
    }

    Ok(())
}

fn expected_spirv_type(kind: ComputeBindingKind) -> RDescriptorType {
    match kind {
        ComputeBindingKind::StorageBuffer => RDescriptorType::STORAGE_BUFFER,
        ComputeBindingKind::UniformBuffer => RDescriptorType::UNIFORM_BUFFER,
        ComputeBindingKind::SampledTexture => RDescriptorType::COMBINED_IMAGE_SAMPLER,
        ComputeBindingKind::StorageImage => RDescriptorType::STORAGE_IMAGE,
    }
}

fn descriptor_kind_to_vk(kind: ComputeBindingKind) -> vk::DescriptorType {
    match kind {
        ComputeBindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        ComputeBindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        ComputeBindingKind::SampledTexture => vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        ComputeBindingKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
    }
}

fn create_shader_module(
    device: &vulkanalia::Device,
    spirv: &[u32],
    label: &str,
) -> Result<vk::ShaderModule> {
    let info = vk::ShaderModuleCreateInfo::builder().code(spirv).build();
    unsafe { device.create_shader_module(&info, None) }.map_err(|e| {
        StreamError::GpuError(format!(
            "Compute kernel '{label}': failed to create shader module: {e}"
        ))
    })
}

fn create_descriptor_set_layout(
    device: &vulkanalia::Device,
    bindings: &[ComputeBindingSpec],
) -> Result<vk::DescriptorSetLayout> {
    let layout_bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .map(|spec| {
            vk::DescriptorSetLayoutBinding::builder()
                .binding(spec.binding)
                .descriptor_type(descriptor_kind_to_vk(spec.kind))
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build()
        })
        .collect();

    let info = vk::DescriptorSetLayoutCreateInfo::builder()
        .bindings(&layout_bindings)
        .build();
    unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create descriptor set layout: {e}")))
}

fn create_pipeline_layout(
    device: &vulkanalia::Device,
    set_layout: vk::DescriptorSetLayout,
    push_constant_size: u32,
) -> Result<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push_constant_ranges: Vec<vk::PushConstantRange> = if push_constant_size > 0 {
        vec![vk::PushConstantRange::builder()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
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
        .map_err(|e| StreamError::GpuError(format!("Failed to create pipeline layout: {e}")))
}

/// Build a compute pipeline, transparently using an on-disk pipeline cache
/// keyed by the SPIR-V SHA-256.
///
/// The driver validates the cache blob's `VkPipelineCacheHeaderVersionOne`
/// (vendor_id + device_id + cache_uuid) and silently rejects mismatches —
/// fallback-to-recompile is automatic. All cache I/O failures are non-fatal:
/// we warn and fall through to a null cache so kernel construction never
/// fails on a stale, corrupt, or unwritable cache file.
fn create_compute_pipeline_with_cache(
    device: &vulkanalia::Device,
    shader_module: vk::ShaderModule,
    pipeline_layout: vk::PipelineLayout,
    spv: &[u8],
    label: &str,
) -> Result<vk::Pipeline> {
    let cache_path = pipeline_cache_file_path(spv);
    let initial_data = cache_path.as_deref().and_then(read_cache_blob);

    // `Some(cache_handle)` if we successfully created a `VkPipelineCache` —
    // we need to destroy it before returning regardless of success/failure
    // of the pipeline build.
    let pipeline_cache = create_pipeline_cache_handle(device, initial_data.as_deref(), label);

    let cache_handle = pipeline_cache.unwrap_or(vk::PipelineCache::null());

    let stage = vk::PipelineShaderStageCreateInfo::builder()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader_module)
        .name(b"main\0")
        .build();
    let info = vk::ComputePipelineCreateInfo::builder()
        .stage(stage)
        .layout(pipeline_layout)
        .build();

    let pipelines_result =
        unsafe { device.create_compute_pipelines(cache_handle, &[info], None) };

    // Persist whatever the driver populated, even if one of the pipelines in
    // a hypothetical multi-pipeline batch failed (we only build one here).
    if pipeline_cache.is_some() {
        if let Some(path) = cache_path.as_deref() {
            persist_pipeline_cache(device, cache_handle, path, label);
        }
        unsafe { device.destroy_pipeline_cache(cache_handle, None) };
    }

    let pipelines = pipelines_result.map_err(|e| {
        StreamError::GpuError(format!(
            "Compute kernel '{label}': failed to create compute pipeline: {e}"
        ))
    })?;
    Ok(pipelines.0[0])
}

/// Resolve the cache directory.
///
/// Order: `STREAMLIB_PIPELINE_CACHE_DIR` env override → `XDG_CACHE_HOME` (via
/// `dirs::cache_dir()`) joined with `streamlib/pipeline-cache` → `None` if no
/// cache root is resolvable. `None` disables caching for this kernel; the
/// kernel still builds, just without `pInitialData`.
fn pipeline_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(PIPELINE_CACHE_DIR_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    dirs::cache_dir().map(|d| d.join("streamlib/pipeline-cache"))
}

/// Compute the cache file path for a given SPIR-V blob, or `None` if no
/// cache directory is resolvable.
///
/// File name is the SHA-256 of the SPIR-V bytes in lowercase hex with a
/// `.bin` suffix. Two SPIR-V blobs that differ by any byte produce
/// distinct file paths; identical blobs hit the same cache file across
/// process restarts.
fn pipeline_cache_file_path(spv: &[u8]) -> Option<PathBuf> {
    let dir = pipeline_cache_dir()?;
    let mut hasher = Sha256::new();
    hasher.update(spv);
    let hash_hex = format!("{:x}", hasher.finalize());
    Some(dir.join(format!("{hash_hex}.bin")))
}

fn read_cache_blob(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                "pipeline cache: unreadable cache file at {}: {e}",
                path.display()
            );
            None
        }
    }
}

fn create_pipeline_cache_handle(
    device: &vulkanalia::Device,
    initial_data: Option<&[u8]>,
    label: &str,
) -> Option<vk::PipelineCache> {
    let mut info = vk::PipelineCacheCreateInfo::builder();
    if let Some(data) = initial_data {
        info = info.initial_data(data);
        tracing::debug!(
            "Compute kernel '{label}': loading pipeline cache (pInitialData {} bytes)",
            data.len()
        );
    } else {
        tracing::debug!(
            "Compute kernel '{label}': pipeline cache cold (no pInitialData)"
        );
    }
    let info = info.build();
    match unsafe { device.create_pipeline_cache(&info, None) } {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!(
                "Compute kernel '{label}': vkCreatePipelineCache failed: {e} — falling back to null cache"
            );
            None
        }
    }
}

fn persist_pipeline_cache(
    device: &vulkanalia::Device,
    cache: vk::PipelineCache,
    path: &Path,
    label: &str,
) {
    let data = match unsafe { device.get_pipeline_cache_data(cache) } {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!(
                "Compute kernel '{label}': vkGetPipelineCacheData failed: {e}"
            );
            return;
        }
    };
    if data.is_empty() {
        // Driver returned no data — nothing to persist.
        return;
    }
    if let Err(e) = atomic_write_pipeline_cache(path, &data) {
        tracing::warn!(
            "Compute kernel '{label}': failed to persist pipeline cache to {}: {e}",
            path.display()
        );
    } else {
        tracing::debug!(
            "Compute kernel '{label}': persisted pipeline cache ({} bytes) to {}",
            data.len(),
            path.display()
        );
    }
}

fn atomic_write_pipeline_cache(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Same-directory temp file → POSIX rename is atomic on the same
    // filesystem. PID + nanos disambiguates concurrent writers; the loser
    // of the race just overwrites the winner, which is fine — both blobs
    // are equally valid and the driver re-validates on next load.
    let suffix = format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut tmp = path.to_path_buf();
    tmp.set_extension(format!(
        "bin.{suffix}"
    ));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn create_descriptor_pool(
    device: &vulkanalia::Device,
    bindings: &[ComputeBindingSpec],
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
        .map_err(|e| StreamError::GpuError(format!("Failed to create descriptor pool: {e}")))
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
        .map_err(|e| StreamError::GpuError(format!("Failed to allocate descriptor set: {e}")))?;
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
        .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {e}")))
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
        .map_err(|e| StreamError::GpuError(format!("Failed to allocate command buffer: {e}")))?;
    Ok(buffers[0])
}

fn vk_buffer_for(buffer: &RhiPixelBuffer) -> Result<(vk::Buffer, vk::DeviceSize)> {
    let inner = &buffer.buffer_ref().inner;
    Ok((inner.buffer(), inner.size()))
}

fn vk_image_view_for(texture: &StreamTexture) -> Result<vk::ImageView> {
    texture.inner.image_view()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::PixelFormat;
    use crate::vulkan::rhi::HostVulkanPixelBuffer;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    /// Allocate a HOST_VISIBLE storage buffer of `element_count * 4` bytes
    /// usable as both an input and output of the test_blend kernel.
    fn make_storage_buffer(
        device: &Arc<HostVulkanDevice>,
        element_count: u32,
    ) -> RhiPixelBuffer {
        let vk_buf = HostVulkanPixelBuffer::new(device, element_count, 1, 4, PixelFormat::Bgra32)
            .expect("Failed to create storage buffer");
        let ref_ = crate::core::rhi::RhiPixelBufferRef {
            inner: Arc::new(vk_buf),
        };
        RhiPixelBuffer::new(ref_)
    }

    fn write_buffer_u32(buf: &RhiPixelBuffer, values: &[u32]) {
        let ptr = buf.buffer_ref().inner.mapped_ptr() as *mut u32;
        unsafe {
            std::ptr::copy_nonoverlapping(values.as_ptr(), ptr, values.len());
        }
    }

    fn read_buffer_u32(buf: &RhiPixelBuffer, len: usize) -> Vec<u32> {
        let ptr = buf.buffer_ref().inner.mapped_ptr() as *const u32;
        let mut out = vec![0u32; len];
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), len);
        }
        out
    }

    fn blend_descriptor(input_count: u32) -> Vec<ComputeBindingSpec> {
        let mut bindings: Vec<ComputeBindingSpec> = (0..input_count)
            .map(ComputeBindingSpec::storage_buffer)
            .collect();
        // Output sits at binding 8 in every variant of test_blend.comp.
        bindings.push(ComputeBindingSpec::storage_buffer(8));
        bindings
    }

    fn blend_spv(input_count: u32) -> &'static [u8] {
        match input_count {
            1 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv")),
            2 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_2.spv")),
            4 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_4.spv")),
            8 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_8.spv")),
            _ => panic!("test_blend.spv variants are only built for 1/2/4/8"),
        }
    }

    fn run_blend_kernel_for(
        device: &Arc<HostVulkanDevice>,
        input_count: u32,
        element_count: u32,
    ) -> (Vec<RhiPixelBuffer>, RhiPixelBuffer) {
        let bindings = blend_descriptor(input_count);
        let kernel = VulkanComputeKernel::new(
            device,
            &ComputeKernelDescriptor {
                label: "test_blend",
                spv: blend_spv(input_count),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel creation");

        let inputs: Vec<RhiPixelBuffer> = (0..input_count)
            .map(|i| {
                let buf = make_storage_buffer(device, element_count);
                let pattern: Vec<u32> = (0..element_count)
                    .map(|j| (j + 1) * (i + 1))
                    .collect();
                write_buffer_u32(&buf, &pattern);
                buf
            })
            .collect();
        let output = make_storage_buffer(device, element_count);

        for (i, buf) in inputs.iter().enumerate() {
            kernel
                .set_storage_buffer(i as u32, buf)
                .expect("set_storage_buffer for input");
        }
        kernel
            .set_storage_buffer(8, &output)
            .expect("set_storage_buffer for output");

        let push: [u32; 1] = [element_count];
        kernel.set_push_constants_value(&push).expect("push constants");

        let group_count_x = element_count.div_ceil(64);
        kernel
            .dispatch(group_count_x, 1, 1)
            .expect("kernel dispatch");

        (inputs, output)
    }

    fn expected_blend(inputs: &[RhiPixelBuffer], element_count: u32) -> Vec<u32> {
        let active: Vec<Vec<u32>> = inputs
            .iter()
            .map(|b| read_buffer_u32(b, element_count as usize))
            .collect();
        (0..element_count as usize)
            .map(|j| active.iter().map(|v| v[j]).sum::<u32>())
            .collect()
    }

    fn assert_blend_for(input_count: u32) {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let elem_count = 256u32;
        let (inputs, output) = run_blend_kernel_for(&device, input_count, elem_count);
        let actual = read_buffer_u32(&output, elem_count as usize);
        let expected = expected_blend(&inputs, elem_count);
        assert_eq!(
            actual, expected,
            "blend output mismatch for input_count={input_count}"
        );
    }

    #[test]
    fn dispatch_matches_expected_blend_for_one_input() {
        assert_blend_for(1);
    }

    #[test]
    fn dispatch_matches_expected_blend_for_two_inputs() {
        assert_blend_for(2);
    }

    #[test]
    fn dispatch_matches_expected_blend_for_four_inputs() {
        assert_blend_for(4);
    }

    #[test]
    fn dispatch_matches_expected_blend_for_eight_inputs() {
        assert_blend_for(8);
    }

    #[test]
    fn kernel_bindings_reflect_descriptor() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        for &input_count in &[1u32, 2, 4, 8] {
            let bindings = blend_descriptor(input_count);
            let kernel = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "binding-shape",
                    spv: blend_spv(input_count),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            )
            .expect("kernel creation");
            assert_eq!(
                kernel.bindings().len(),
                input_count as usize + 1,
                "expected {input_count}+1 bindings for {input_count}-input variant"
            );
            assert_eq!(kernel.push_constant_size(), 4);
            for spec in kernel.bindings() {
                assert_eq!(spec.kind, ComputeBindingKind::StorageBuffer);
            }
        }
    }

    #[test]
    fn rejects_descriptor_with_mismatched_binding_kind() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // SPIR-V binding 0 is StorageBuffer — declaring it as UniformBuffer must fail.
        let bindings = vec![
            ComputeBindingSpec::uniform_buffer(0),
            ComputeBindingSpec::storage_buffer(8),
        ];
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "kind-mismatch",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 0") && msg.contains("UniformBuffer"),
            "expected mismatch error mentioning binding 0 and UniformBuffer, got: {msg}"
        );
    }

    #[test]
    fn rejects_descriptor_missing_a_binding_the_shader_declares() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // 4-input shader declares bindings 0..3 + 8; omit binding 2.
        let bindings = vec![
            ComputeBindingSpec::storage_buffer(0),
            ComputeBindingSpec::storage_buffer(1),
            ComputeBindingSpec::storage_buffer(3),
            ComputeBindingSpec::storage_buffer(8),
        ];
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "missing-binding",
                spv: blend_spv(4),
                bindings: &bindings,
                push_constant_size: 4,
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 2"),
            "expected error about missing binding 2, got: {msg}"
        );
    }

    #[test]
    fn rejects_descriptor_with_extra_binding_not_in_shader() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // 1-input shader declares only bindings 0 and 8 — extra binding 1 is invalid.
        let bindings = vec![
            ComputeBindingSpec::storage_buffer(0),
            ComputeBindingSpec::storage_buffer(1),
            ComputeBindingSpec::storage_buffer(8),
        ];
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "extra-binding",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 1"),
            "expected error about extra binding 1, got: {msg}"
        );
    }

    #[test]
    fn rejects_push_constant_size_mismatch() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = blend_descriptor(1);
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "push-size-mismatch",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 16, // SPIR-V declares 4 bytes
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("push-constant size mismatch"),
            "expected push-constant size error, got: {msg}"
        );
    }

    #[test]
    fn dispatch_without_setting_bindings_fails_loud() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = blend_descriptor(2);
        let kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "missing-set",
                spv: blend_spv(2),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel creation");
        kernel.set_push_constants_value(&[1u32]).expect("push constants");
        let err = kernel.dispatch(1, 1, 1).err().expect("expected dispatch failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("not set before dispatch"),
            "expected missing-binding error, got: {msg}"
        );
    }

    #[test]
    fn dispatch_completes_within_reasonable_budget() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // Performance smoke: a small kernel build + first dispatch should
        // round-trip in well under a couple seconds on any reasonable GPU.
        // Catches catastrophic regressions like accidentally recreating
        // Vulkan objects per dispatch. The budget is loose enough to absorb
        // queue-mutex contention from sibling kernel tests running in
        // parallel and cold-pipeline-cache compilation on the first run.
        let elem_count = 4096u32;
        let start = std::time::Instant::now();
        let (_inputs, _output) = run_blend_kernel_for(&device, 8, elem_count);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "kernel build + first dispatch took {elapsed:?} (>1500ms); regression?"
        );
    }

    // ---- Pipeline cache tests ------------------------------------------------
    //
    // Every test that mutates `STREAMLIB_PIPELINE_CACHE_DIR` is marked
    // `#[serial(streamlib_pipeline_cache_env)]` so they serialize against
    // each other regardless of whether they skip the GPU device queue
    // mutex. The `pipeline_cache_dir()` resolution path doesn't need a
    // Vulkan device, so the queue-mutex serialization isn't sufficient
    // by itself. Required for soundness under Rust 2024's `unsafe
    // std::env::set_var` — concurrent reads from sibling test threads
    // would otherwise be UB.

    use serial_test::serial;

    fn unique_cache_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "streamlib-pipeline-cache-{label}-{}-{}",
            std::process::id(),
            nanos
        ))
    }

    /// Run `f` with `STREAMLIB_PIPELINE_CACHE_DIR` set to `dir`, restoring
    /// the previous value (or unsetting) afterward. Callers MUST mark
    /// the test `#[serial(streamlib_pipeline_cache_env)]` — see the
    /// section comment above for why.
    fn with_pipeline_cache_dir<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
        let prev = std::env::var(PIPELINE_CACHE_DIR_ENV).ok();
        // SAFETY: callers serialize via `#[serial(streamlib_pipeline_cache_env)]`,
        // so concurrent env-var reads/writes from sibling tests are not
        // possible.
        unsafe { std::env::set_var(PIPELINE_CACHE_DIR_ENV, dir) };
        let r = f();
        // SAFETY: same as above.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(PIPELINE_CACHE_DIR_ENV, v),
                None => std::env::remove_var(PIPELINE_CACHE_DIR_ENV),
            }
        }
        r
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn cache_file_path_is_stable_for_same_spirv_and_distinct_for_different() {
        let dir = unique_cache_dir("path-stability");
        with_pipeline_cache_dir(&dir, || {
            let p1 = pipeline_cache_file_path(blend_spv(1)).expect("dir resolves");
            let p2 = pipeline_cache_file_path(blend_spv(1)).expect("dir resolves");
            let p4 = pipeline_cache_file_path(blend_spv(4)).expect("dir resolves");
            assert_eq!(p1, p2, "same SPIR-V must hash to same path");
            assert_ne!(p1, p4, "different SPIR-V must hash to different paths");
            assert!(p1.starts_with(&dir));
            assert!(
                p1.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with(".bin"))
                    .unwrap_or(false),
                "cache file should end in .bin, got {p1:?}"
            );
        });
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn env_override_takes_precedence_over_xdg_default() {
        let dir = unique_cache_dir("env-override");
        with_pipeline_cache_dir(&dir, || {
            let resolved = pipeline_cache_dir().expect("env var resolves");
            assert_eq!(resolved, dir);
        });
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn empty_env_var_falls_through_to_xdg_default() {
        // Whatever the XDG default resolves to is platform-dependent — what we
        // assert is that an explicitly-empty override does NOT short-circuit
        // resolution to an empty path.
        let dir = unique_cache_dir("empty-env");
        let prev = std::env::var(PIPELINE_CACHE_DIR_ENV).ok();
        // SAFETY: serialized via `#[serial(streamlib_pipeline_cache_env)]`
        // above, so concurrent env-var reads/writes from sibling tests
        // are not possible.
        unsafe { std::env::set_var(PIPELINE_CACHE_DIR_ENV, "") };
        let resolved = pipeline_cache_dir();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(PIPELINE_CACHE_DIR_ENV, v),
                None => std::env::remove_var(PIPELINE_CACHE_DIR_ENV),
            }
        }
        // Either dirs::cache_dir() resolves on this host (and we get a
        // non-empty path) or it doesn't (and we get None) — either way is
        // valid; what's invalid is the env var producing the literal "".
        if let Some(p) = resolved {
            assert!(!p.as_os_str().is_empty());
            assert_ne!(p, PathBuf::from(""));
        }
        let _ = dir; // keep the helper's tempdir name in scope
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn cache_miss_writes_cache_file_after_kernel_construction() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("miss-writes");
        with_pipeline_cache_dir(&dir, || {
            assert!(
                !dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(),
                "tempdir should be empty before kernel construction"
            );
            let bindings = blend_descriptor(1);
            let _kernel = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "cache-miss",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            )
            .expect("kernel creation");
            let expected = pipeline_cache_file_path(blend_spv(1)).expect("path");
            assert!(
                expected.exists(),
                "cache file {} should exist after construction",
                expected.display()
            );
            let written = std::fs::read(&expected).expect("read cache");
            assert!(!written.is_empty(), "cache file must not be empty");
            // Header sanity: VkPipelineCacheHeaderVersionOne is 32 bytes,
            // first u32 is header_size = 32, second u32 is header_version = 1.
            let len = u32::from_le_bytes([
                written[0], written[1], written[2], written[3],
            ]);
            let ver = u32::from_le_bytes([
                written[4], written[5], written[6], written[7],
            ]);
            assert!(
                len >= 16 && ver == 1,
                "expected pipeline cache header (len, ver=1), got len={len} ver={ver}"
            );
        });
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn cache_hit_does_not_panic_or_break_kernel_construction() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("hit-reuses");
        with_pipeline_cache_dir(&dir, || {
            let bindings = blend_descriptor(1);
            // First construction populates the cache.
            drop(
                VulkanComputeKernel::new(
                    &device,
                    &ComputeKernelDescriptor {
                        label: "cache-hit/first",
                        spv: blend_spv(1),
                        bindings: &bindings,
                        push_constant_size: 4,
                    },
                )
                .expect("first construction"),
            );
            let cache_path = pipeline_cache_file_path(blend_spv(1)).expect("path");
            let warm_blob = std::fs::read(&cache_path).expect("warm read");
            assert!(!warm_blob.is_empty(), "cache must be populated by first run");

            // Second construction should hit the cache. We can't assert on
            // tracing output deterministically, but we can assert that the
            // file still exists and is non-empty — the warm path read it,
            // built the pipeline, and rewrote (atomic) on success.
            drop(
                VulkanComputeKernel::new(
                    &device,
                    &ComputeKernelDescriptor {
                        label: "cache-hit/second",
                        spv: blend_spv(1),
                        bindings: &bindings,
                        push_constant_size: 4,
                    },
                )
                .expect("second construction"),
            );
            let after = std::fs::read(&cache_path).expect("post-warm read");
            assert!(!after.is_empty());
        });
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn corrupt_cache_blob_falls_back_to_recompile_and_overwrites() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("corrupt-blob");
        with_pipeline_cache_dir(&dir, || {
            std::fs::create_dir_all(&dir).expect("mkdir");
            let cache_path = pipeline_cache_file_path(blend_spv(1)).expect("path");
            // Plant a header-invalid blob that the driver will reject.
            // 32 bytes of zeros has header_version=0 ≠ 1 — driver ignores
            // the data and treats the cache as empty.
            std::fs::write(&cache_path, vec![0u8; 32]).expect("plant corrupt blob");

            let bindings = blend_descriptor(1);
            let kernel = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "corrupt-blob",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            )
            .expect("kernel must construct despite corrupt cache");
            drop(kernel);

            let after = std::fs::read(&cache_path).expect("post-recompile read");
            // Driver-rewritten blob has header_version=1 in bytes 4..8.
            let ver = u32::from_le_bytes([after[4], after[5], after[6], after[7]]);
            assert_eq!(
                ver, 1,
                "expected driver to rewrite cache with valid header, got version={ver}"
            );
        });
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn read_only_cache_dir_does_not_break_kernel_construction() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("readonly-dir");
        with_pipeline_cache_dir(&dir, || {
            std::fs::create_dir_all(&dir).expect("mkdir");
            let mut perms = std::fs::metadata(&dir).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // r-x for owner only — write attempts fail with EACCES.
                perms.set_mode(0o555);
            }
            std::fs::set_permissions(&dir, perms).expect("set readonly");

            let bindings = blend_descriptor(1);
            let result = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "readonly-cache",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            );

            // Restore so the cleanup can rmdir.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut p = std::fs::metadata(&dir).unwrap().permissions();
                p.set_mode(0o755);
                let _ = std::fs::set_permissions(&dir, p);
            }

            assert!(
                result.is_ok(),
                "kernel construction must succeed even when cache_dir is read-only: {result:?}"
            );
        });
    }
}

