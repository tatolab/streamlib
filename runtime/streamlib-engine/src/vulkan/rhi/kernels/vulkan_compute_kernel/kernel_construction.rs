// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{ComputeBindingKind, ComputeKernelDescriptor};
use crate::core::{Error, Result};
use crate::vulkan::rhi::HostVulkanDevice;
use crate::vulkan::rhi::kernels::vulkan_kernel_samplers::{
    SamplerOfABinding, VulkanKernelSamplers, refuse_samplers_the_kernel_cannot_take,
};

use super::pipeline_creation::{
    allocate_descriptor_set, create_compute_pipeline_with_cache, create_descriptor_pool,
    create_descriptor_set_layout, create_dispatch_slots, create_pipeline_layout,
    create_shader_module,
};
use super::spirv_validation::validate_against_spirv;
use super::{PendingState, VulkanComputeKernelInner};

impl VulkanComputeKernelInner {
    /// Create a new compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Reflects the SPIR-V via `rspirv-reflect`, validates that the declared
    /// `bindings` match the shader's descriptor types, and rejects any mismatch
    /// before allocating Vulkan objects.
    pub(crate) fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
    ) -> Result<Self> {
        Self::new_caching_its_pipeline_in(vulkan_device, descriptor, None)
    }

    /// [`Self::new`], keeping the pipeline's on-disk cache in the stream's
    /// `pipeline_cache_directory_of_its_stream` when one is given.
    pub(crate) fn new_caching_its_pipeline_in(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
        pipeline_cache_directory_of_its_stream: Option<&Path>,
    ) -> Result<Self> {
        Self::new_inner(
            vulkan_device,
            descriptor,
            &[],
            pipeline_cache_directory_of_its_stream,
        )
    }

    /// Variant of [`Self::new`] that bakes one or more immutable
    /// samplers into the descriptor-set layout, via `pImmutableSamplers`.
    ///
    /// Each `(binding, sampler)` pair attaches `sampler` to the matching
    /// declared binding at layout-creation time; the binding must be
    /// declared as [`ComputeBindingKind::SampledTexture`] (combined
    /// image-sampler, the only shape Vulkan permits an immutable sampler
    /// to bake into for compute today).
    ///
    /// **Lifetime contract.** The caller owns each `vk::Sampler` and is
    /// responsible for keeping it alive for the kernel's lifetime —
    /// the kernel records the handle in the layout but does not
    /// duplicate or refcount it. Dropping a sampler before the kernel
    /// is undefined behavior.
    ///
    /// Callers binding an immutable-sampler slot per-frame use
    /// [`Self::set_combined_image_sampler_view`], which writes the
    /// image view only — Vulkan ignores the sampler field in the write
    /// whenever the layout slot has an immutable sampler.
    pub(crate) fn new_with_immutable_samplers(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
        immutable_samplers: &[(u32, vk::Sampler)],
    ) -> Result<Self> {
        // Pre-validate: every (binding, sampler) pair must refer to a
        // declared SampledTexture binding. We catch this before going to
        // Vulkan so the error names the offending binding directly.
        for (binding, _) in immutable_samplers {
            let spec = descriptor
                .bindings
                .iter()
                .find(|b| b.binding == *binding)
                .ok_or_else(|| {
                    Error::GpuError(format!(
                        "Compute kernel '{}': immutable sampler refers to undeclared binding {}",
                        descriptor.label, binding
                    ))
                })?;
            if spec.kind != ComputeBindingKind::SampledTexture {
                return Err(Error::GpuError(format!(
                    "Compute kernel '{}': immutable sampler attached to binding {} \
                     declared as {:?}; only `SampledTexture` is supported",
                    descriptor.label, binding, spec.kind
                )));
            }
            if spec.sampler.is_some() {
                return Err(Error::GpuError(format!(
                    "Compute kernel '{}': binding {binding} declares a sampler and is given an \
                     immutable one; the immutable sampler would win, so declare only one",
                    descriptor.label
                )));
            }
        }
        Self::new_inner(vulkan_device, descriptor, immutable_samplers, None)
    }

    fn new_inner(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
        immutable_samplers: &[(u32, vk::Sampler)],
        pipeline_cache_directory_of_its_stream: Option<&Path>,
    ) -> Result<Self> {
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();
        let device = vulkan_device.device();

        let reconciled_bindings =
            validate_against_spirv(descriptor, vulkan_device.subgroup_operation_support())?;
        refuse_samplers_the_kernel_cannot_take(
            &format!("Compute kernel '{}'", descriptor.label),
            reconciled_bindings.iter().map(|spec| SamplerOfABinding {
                binding: spec.binding,
                samples_a_texture: spec.kind == ComputeBindingKind::SampledTexture,
                sampler: spec.sampler,
            }),
            vulkan_device.sampler_anisotropy_limit(),
        )?;

        let spirv: Vec<u32> = descriptor
            .spv
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        // ---- Vulkan objects ----------------------------------------------------------
        // Created in a strict order; on failure earlier objects are torn down by the
        // staged-cleanup helpers so we never leak on the error path.

        let shader_module = create_shader_module(device, &spirv, descriptor.label)?;

        let descriptor_set_layout =
            match create_descriptor_set_layout(device, descriptor.bindings, immutable_samplers) {
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

        // A NUL inside the name would truncate the entry point Vulkan looks
        // for, so it is refused rather than silently shortened.
        let entry_point = std::ffi::CString::new(descriptor.entry_point).map_err(|_| {
            Error::GpuError(format!(
                "compute kernel `{}` declares an entry point containing a NUL byte: {:?}",
                descriptor.label, descriptor.entry_point
            ))
        })?;
        let pipeline = match create_compute_pipeline_with_cache(
            device,
            shader_module,
            pipeline_layout,
            descriptor.spv,
            &entry_point,
            descriptor.specialization_constants,
            descriptor.label,
            pipeline_cache_directory_of_its_stream,
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

        let record_descriptor_set =
            match allocate_descriptor_set(device, descriptor_pool, descriptor_set_layout) {
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

        let dispatch_slots = match create_dispatch_slots(
            device,
            descriptor_pool,
            descriptor_set_layout,
            queue_family_index,
        ) {
            Ok(slots) => slots,
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

        Ok(Self {
            label: descriptor.label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            device: device.clone(),
            queue,
            bindings: reconciled_bindings,
            push_constant_size: descriptor.push_constant_size,
            pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            record_descriptor_set,
            shader_module,
            dispatch_slots: dispatch_slots.into_iter().map(Mutex::new).collect(),
            next_dispatch_slot: AtomicUsize::new(0),
            samplers: VulkanKernelSamplers::new(
                device.clone(),
                format!("Compute kernel '{}'", descriptor.label),
            ),
            immutable_sampler_bindings: immutable_samplers.iter().map(|(b, _)| *b).collect(),
            pending: Mutex::new(PendingState {
                bindings: HashMap::new(),
                push_constants: Vec::new(),
            }),
        })
    }
}
