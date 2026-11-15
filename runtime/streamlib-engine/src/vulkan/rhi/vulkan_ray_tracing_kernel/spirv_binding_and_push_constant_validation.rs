// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Validating a ray-tracing kernel's declared bindings and push constants against every stage's SPIR-V.

use crate::core::rhi::{
    RayTracingBindingKind, RayTracingBindingSpec, RayTracingKernelDescriptor,
    RayTracingShaderStageFlags, ray_tracing_spirv_type_to_kind,
    reflect_spirv_without_its_debug_source_strings, refuse_a_binding_the_shader_left_unnamed,
    refuse_a_descriptor_set_other_than_set_0, refuse_one_binding_name_that_identifies_two_slots,
    refuse_one_binding_slot_two_stages_spell_differently,
};
use crate::core::{Error, Result};

use crate::vulkan::rhi::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;

use super::{stage_to_stage_flag, stage_to_vk};

/// Validate the declaration against every stage and return the specs with the
/// shader's own binding names adopted onto them.
pub(super) fn validate_bindings_against_spirv(
    descriptor: &RayTracingKernelDescriptor<'_>,
    subgroup_operation_support: &VulkanSubgroupOperationSupport,
) -> Result<Vec<RayTracingBindingSpec>> {
    use std::collections::BTreeMap;

    let kernel_kind_label = format!("Ray-tracing kernel '{}'", descriptor.label);

    // Merge per-stage SPIR-V reflection into a single map.
    let mut merged: BTreeMap<u32, (RayTracingBindingKind, RayTracingShaderStageFlags, String)> =
        BTreeMap::new();

    for stage in descriptor.stages {
        let reflection =
            reflect_spirv_without_its_debug_source_strings(stage.spv).map_err(|e| {
                Error::GpuError(format!(
                    "Ray-tracing kernel '{}': failed to reflect SPIR-V for stage {:?}: {e:?}",
                    descriptor.label, stage.stage
                ))
            })?;
        subgroup_operation_support.refuse_a_shader_the_driver_cannot_serve(
            &kernel_kind_label,
            stage_to_vk(stage.stage),
            &reflection.0,
        )?;
        let sets = reflection.get_descriptor_sets().map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{}': failed to extract descriptor sets for stage {:?}: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        refuse_a_descriptor_set_other_than_set_0(
            &kernel_kind_label,
            Some(stage.stage),
            sets.keys().copied(),
        )?;
        let stage_flag = stage_to_stage_flag(stage.stage);
        if let Some(set0) = sets.get(&0) {
            for (&binding, info) in set0 {
                let kind = ray_tracing_spirv_type_to_kind(info.ty).ok_or_else(|| {
                    Error::GpuError(format!(
                        "Ray-tracing kernel '{}': SPIR-V binding {} in stage {:?} has unsupported descriptor type {:?}",
                        descriptor.label, binding, stage.stage, info.ty
                    ))
                })?;
                refuse_a_binding_the_shader_left_unnamed(
                    &kernel_kind_label,
                    stage.stage,
                    binding,
                    kind,
                    &info.name,
                )?;
                let entry = merged.entry(binding).or_insert((
                    kind,
                    RayTracingShaderStageFlags::NONE,
                    info.name.clone(),
                ));
                if entry.0 != kind {
                    return Err(Error::GpuError(format!(
                        "Ray-tracing kernel '{}': SPIR-V binding {} declared as {:?} in one stage and {:?} in another",
                        descriptor.label, binding, entry.0, kind
                    )));
                }
                refuse_one_binding_slot_two_stages_spell_differently(
                    &kernel_kind_label,
                    stage.stage,
                    binding,
                    &entry.2,
                    &info.name,
                )?;
                entry.1 |= stage_flag;
            }
        }
    }

    refuse_one_binding_name_that_identifies_two_slots(
        &kernel_kind_label,
        merged
            .iter()
            .map(|(&binding, (_, _, name))| (binding, name.as_str())),
    )?;

    // Every declared binding must exist in the merged SPIR-V map. The shader's
    // own name for the binding is adopted onto the returned spec — that is what
    // a by-name dispatch resolves against, and the shader is its source of
    // truth.
    let mut reconciled: Vec<RayTracingBindingSpec> = Vec::with_capacity(descriptor.bindings.len());
    for spec in descriptor.bindings {
        let (spirv_kind, spirv_stages, spirv_name) =
            merged.get(&spec.binding).ok_or_else(|| {
                Error::GpuError(format!(
                    "Ray-tracing kernel '{}': binding {} declared but missing in SPIR-V",
                    descriptor.label, spec.binding
                ))
            })?;
        if *spirv_kind != spec.kind {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} declared {:?}, but SPIR-V has {:?}",
                descriptor.label, spec.binding, spec.kind, spirv_kind
            )));
        }
        if !spec.stages.contains(*spirv_stages) {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} stages {:?} miss SPIR-V usage in {:?}",
                descriptor.label, spec.binding, spec.stages, spirv_stages
            )));
        }
        if let Some(declared_name) = spec.name.as_deref()
            && declared_name != spirv_name
        {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} declared name `{}`, but SPIR-V names it `{}`",
                descriptor.label, spec.binding, declared_name, spirv_name
            )));
        }
        let mut adopted = spec.clone();
        adopted.name = Some(std::borrow::Cow::Owned(spirv_name.clone()));
        reconciled.push(adopted);
    }

    // Conversely, every SPIR-V binding must be declared.
    for (&binding, (kind, _, name)) in &merged {
        if !descriptor.bindings.iter().any(|s| s.binding == binding) {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': SPIR-V declares binding {} ({:?}, name `{}`) but it is missing from the descriptor",
                descriptor.label, binding, kind, name
            )));
        }
    }

    Ok(reconciled)
}

pub(super) fn validate_push_constants_against_spirv(
    descriptor: &RayTracingKernelDescriptor<'_>,
) -> Result<()> {
    let mut max_size = 0u32;
    for stage in descriptor.stages {
        let reflection = reflect_spirv_without_its_debug_source_strings(stage.spv).map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{}': failed to reflect SPIR-V (push) for stage {:?}: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        if let Some(info) = reflection.get_push_constant_range().map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{}': failed to read push-constant range for stage {:?}: {e:?}",
                descriptor.label, stage.stage
            ))
        })? {
            max_size = max_size.max(info.size);
        }
    }
    if max_size != descriptor.push_constants.size {
        return Err(Error::GpuError(format!(
            "Ray-tracing kernel '{}': push-constant size mismatch — SPIR-V says {}, descriptor declares {}",
            descriptor.label, max_size, descriptor.push_constants.size
        )));
    }
    Ok(())
}
