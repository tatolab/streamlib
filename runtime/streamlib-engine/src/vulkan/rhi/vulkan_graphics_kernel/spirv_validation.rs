// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::rhi::{
    GraphicsBindingKind, GraphicsBindingSpec, GraphicsKernelDescriptor, GraphicsShaderStage,
    GraphicsShaderStageFlags, reflect_spirv_without_its_debug_source_strings,
    refuse_a_binding_the_shader_left_unnamed, refuse_a_descriptor_set_other_than_set_0,
    refuse_one_binding_name_that_identifies_two_slots,
    refuse_one_binding_slot_two_stages_spell_differently,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;

use super::pipeline_state_conversions::graphics_stage_to_vk;
use rspirv_reflect::DescriptorType as RDescriptorType;

// ---- Validation + creation helpers --------------------------------------------

/// Validate the declaration against the shaders and return the specs with the
/// shader's own binding names adopted onto them.
pub(super) fn validate_against_spirv(
    descriptor: &GraphicsKernelDescriptor<'_>,
    subgroup_operation_support: &VulkanSubgroupOperationSupport,
) -> Result<Vec<GraphicsBindingSpec>> {
    use std::collections::BTreeMap;

    let kernel_kind_label = format!("Graphics kernel '{}'", descriptor.label);
    let mut merged: BTreeMap<u32, (RDescriptorType, GraphicsShaderStageFlags, String)> =
        BTreeMap::new();
    let mut spirv_push_size: u32 = 0;
    let mut spirv_push_stages = GraphicsShaderStageFlags::NONE;

    for stage in descriptor.stages {
        let stage_flag = stage_to_flag(stage.stage);
        let reflection =
            reflect_spirv_without_its_debug_source_strings(stage.spv).map_err(|e| {
                Error::GpuError(format!(
                    "Graphics kernel '{}': failed to reflect SPIR-V for {:?} stage: {e:?}",
                    descriptor.label, stage.stage
                ))
            })?;
        subgroup_operation_support.refuse_a_shader_the_driver_cannot_serve(
            &kernel_kind_label,
            graphics_stage_to_vk(stage.stage),
            &reflection.0,
        )?;
        let sets = reflection.get_descriptor_sets().map_err(|e| {
            Error::GpuError(format!(
                "Graphics kernel '{}': failed to extract descriptor sets for {:?} stage: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        refuse_a_descriptor_set_other_than_set_0(
            &kernel_kind_label,
            Some(stage.stage),
            sets.keys().copied(),
        )?;
        if let Some(set0) = sets.get(&0) {
            for (&binding, info) in set0 {
                refuse_a_binding_the_shader_left_unnamed(
                    &kernel_kind_label,
                    stage.stage,
                    binding,
                    info.ty,
                    &info.name,
                )?;
                let entry = merged.entry(binding).or_insert((
                    info.ty,
                    GraphicsShaderStageFlags::NONE,
                    info.name.clone(),
                ));
                if entry.0 != info.ty {
                    return Err(Error::GpuError(format!(
                        "Graphics kernel '{}': binding {binding} type conflict — {:?} vs {:?} (in {:?})",
                        descriptor.label, entry.0, info.ty, stage.stage
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
        if let Some(info) = reflection.get_push_constant_range().map_err(|e| {
            Error::GpuError(format!(
                "Graphics kernel '{}': failed to read push-constant range for {:?} stage: {e:?}",
                descriptor.label, stage.stage
            ))
        })? {
            spirv_push_size = spirv_push_size.max(info.size);
            spirv_push_stages |= stage_flag;
        }
    }

    refuse_one_binding_name_that_identifies_two_slots(
        &kernel_kind_label,
        merged
            .iter()
            .map(|(&binding, (_, _, name))| (binding, name.as_str())),
    )?;

    // Each declared binding must agree with merged shader declaration. The
    // shader's own name for the binding is adopted onto the returned spec —
    // that is what a by-name draw resolves against, and the shader is its
    // source of truth.
    let mut reconciled: Vec<GraphicsBindingSpec> = Vec::with_capacity(descriptor.bindings.len());
    for spec in descriptor.bindings {
        let merged_entry = merged.get(&spec.binding).ok_or_else(|| {
            Error::GpuError(format!(
                "Graphics kernel '{}': binding {} declared but missing in SPIR-V",
                descriptor.label, spec.binding
            ))
        })?;
        let expected = expected_spirv_type(spec.kind);
        if merged_entry.0 != expected {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': binding {} declared {:?} ({:?}), but SPIR-V has {:?}",
                descriptor.label, spec.binding, spec.kind, expected, merged_entry.0
            )));
        }
        if let Some(declared_name) = spec.name.as_deref()
            && declared_name != merged_entry.2
        {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': binding {} declared name `{}`, but SPIR-V names it `{}`",
                descriptor.label, spec.binding, declared_name, merged_entry.2
            )));
        }
        // Declared visibility must cover at least the SPIR-V's stages —
        // i.e., declared stages ⊇ shader stages. A binding consumed in
        // fragment but declared visible only to vertex would be a Vulkan
        // validation error at draw time; reject up front.
        if (merged_entry.1.bits() & !spec.stages.bits()) != 0 {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': binding {} stage visibility mismatch — declared {:?}, SPIR-V uses bits {:#b}",
                descriptor.label,
                spec.binding,
                spec.stages.bits(),
                merged_entry.1.bits()
            )));
        }
        let mut adopted = spec.clone();
        adopted.name = Some(std::borrow::Cow::Owned(merged_entry.2.clone()));
        reconciled.push(adopted);
    }

    // Conversely, every SPIR-V binding must be declared.
    for (&binding, (ty, stages, name)) in &merged {
        if !descriptor.bindings.iter().any(|s| s.binding == binding) {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': SPIR-V declares binding {} ({:?}, name `{}`, stages {:#b}) but it is missing from the descriptor",
                descriptor.label,
                binding,
                ty,
                name,
                stages.bits()
            )));
        }
    }

    // Push-constant size must match.
    if spirv_push_size != descriptor.push_constants.size {
        return Err(Error::GpuError(format!(
            "Graphics kernel '{}': push-constant size mismatch — SPIR-V has {} bytes (stages {:#b}), descriptor declares {} bytes",
            descriptor.label,
            spirv_push_size,
            spirv_push_stages.bits(),
            descriptor.push_constants.size
        )));
    }
    if spirv_push_size > 0
        && (spirv_push_stages.bits() & !descriptor.push_constants.stages.bits()) != 0
    {
        return Err(Error::GpuError(format!(
            "Graphics kernel '{}': push-constant stage visibility mismatch — declared {:#b}, SPIR-V uses {:#b}",
            descriptor.label,
            descriptor.push_constants.stages.bits(),
            spirv_push_stages.bits()
        )));
    }

    Ok(reconciled)
}

fn stage_to_flag(stage: GraphicsShaderStage) -> GraphicsShaderStageFlags {
    match stage {
        GraphicsShaderStage::Vertex => GraphicsShaderStageFlags::VERTEX,
        GraphicsShaderStage::Fragment => GraphicsShaderStageFlags::FRAGMENT,
    }
}

fn expected_spirv_type(kind: GraphicsBindingKind) -> RDescriptorType {
    match kind {
        GraphicsBindingKind::StorageBuffer => RDescriptorType::STORAGE_BUFFER,
        GraphicsBindingKind::UniformBuffer => RDescriptorType::UNIFORM_BUFFER,
        GraphicsBindingKind::SampledTexture => RDescriptorType::COMBINED_IMAGE_SAMPLER,
        GraphicsBindingKind::StorageImage => RDescriptorType::STORAGE_IMAGE,
    }
}
