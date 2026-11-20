// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::vk;

use rspirv_reflect::DescriptorType as RDescriptorType;

use crate::core::rhi::{
    ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor,
    reflect_spirv_without_what_rspirv_refuses, refuse_a_descriptor_set_other_than_set_0,
    refuse_specialization_constants_the_shader_does_not_take,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::kernels::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;

/// Validate the declaration against the shader and return the specs with the
/// shader's own binding names adopted onto them.
pub(super) fn validate_against_spirv(
    descriptor: &ComputeKernelDescriptor<'_>,
    subgroup_operation_support: &VulkanSubgroupOperationSupport,
) -> Result<Vec<ComputeBindingSpec>> {
    let reflection = reflect_spirv_without_what_rspirv_refuses(descriptor.spv).map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{}': failed to reflect SPIR-V: {e:?}",
            descriptor.label
        ))
    })?;
    subgroup_operation_support.refuse_a_shader_the_driver_cannot_serve(
        &format!("Compute kernel '{}'", descriptor.label),
        vk::ShaderStageFlags::COMPUTE,
        &reflection.0,
    )?;

    refuse_specialization_constants_the_shader_does_not_take(
        &format!("Compute kernel '{}'", descriptor.label),
        descriptor.specialization_constants,
        &reflection.0,
    )?;

    let sets = reflection.get_descriptor_sets().map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{}': failed to extract descriptor sets: {e:?}",
            descriptor.label
        ))
    })?;

    refuse_a_descriptor_set_other_than_set_0(
        &format!("Compute kernel '{}'", descriptor.label),
        None::<&str>,
        sets.keys().copied(),
    )?;

    let set0 = sets.get(&0);

    // For each declared binding, the SPIR-V must agree on (a) presence and (b) descriptor type.
    // The shader's own name for the binding is adopted onto the returned spec — that is what a
    // by-name dispatch resolves against, and the shader is its source of truth.
    let mut reconciled: Vec<ComputeBindingSpec> = Vec::with_capacity(descriptor.bindings.len());
    for spec in descriptor.bindings {
        let info = set0.and_then(|m| m.get(&spec.binding)).ok_or_else(|| {
            Error::GpuError(format!(
                "Compute kernel '{}': binding {} declared but missing in SPIR-V",
                descriptor.label, spec.binding
            ))
        })?;
        let expected = expected_spirv_type(spec.kind);
        if info.ty != expected {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': binding {} declared {:?} ({:?}), but SPIR-V has {:?}",
                descriptor.label, spec.binding, spec.kind, expected, info.ty
            )));
        }
        if let Some(declared_name) = spec.name.as_deref()
            && declared_name != info.name
        {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': binding {} declared name `{}`, but SPIR-V names it `{}`",
                descriptor.label, spec.binding, declared_name, info.name
            )));
        }
        let mut adopted = spec.clone();
        if !info.name.is_empty() {
            adopted.name = Some(std::borrow::Cow::Owned(info.name.clone()));
        }
        reconciled.push(adopted);
    }

    // Conversely, every binding the SPIR-V declares must be present in the
    // declaration — silently leaving a shader binding unset is undefined behavior.
    if let Some(set0) = set0 {
        for (&binding, info) in set0 {
            if !descriptor.bindings.iter().any(|s| s.binding == binding) {
                return Err(Error::GpuError(format!(
                    "Compute kernel '{}': SPIR-V declares binding {} ({:?}, name `{}`) but it is missing from the descriptor",
                    descriptor.label, binding, info.ty, info.name
                )));
            }
        }
    }

    // Push-constant size must match if the shader uses any.
    let push = reflection.get_push_constant_range().map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{}': failed to read push-constant range: {e:?}",
            descriptor.label
        ))
    })?;
    match (push, descriptor.push_constant_size) {
        (Some(info), declared) if info.size != declared => {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': push-constant size mismatch — SPIR-V says {}, descriptor declares {}",
                descriptor.label, info.size, declared
            )));
        }
        (None, declared) if declared > 0 => {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': descriptor declares {} push-constant bytes but SPIR-V has none",
                descriptor.label, declared
            )));
        }
        _ => {}
    }

    Ok(reconciled)
}

fn expected_spirv_type(kind: ComputeBindingKind) -> RDescriptorType {
    match kind {
        ComputeBindingKind::StorageBuffer => RDescriptorType::STORAGE_BUFFER,
        ComputeBindingKind::UniformBuffer => RDescriptorType::UNIFORM_BUFFER,
        ComputeBindingKind::SampledTexture => RDescriptorType::COMBINED_IMAGE_SAMPLER,
        ComputeBindingKind::SampledImage => RDescriptorType::SAMPLED_IMAGE,
        ComputeBindingKind::StorageImage => RDescriptorType::STORAGE_IMAGE,
    }
}
