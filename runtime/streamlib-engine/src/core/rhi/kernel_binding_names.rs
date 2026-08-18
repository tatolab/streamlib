// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Resolving a kernel's descriptor bindings by the shader's own names.
//!
//! Every pipeline kind reads its binding names out of SPIR-V reflection and
//! checks a caller's declaration against them rather than letting the
//! declaration replace them. Compute has no stage axis and both derives and
//! reconciles in [`super::compute_kernel`]; graphics and ray tracing scope each
//! binding to the stages that read it, and share both the multi-stage
//! reflection merge and the reconciliation here because the only thing that
//! differs between them is which newtypes they name. The refusals every
//! pipeline kind owes its callers live here too, compute included.

use std::collections::BTreeMap;

use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};

use crate::core::{Error, Result};

/// Render a shader's declared binding names for an error message.
pub fn quote_declared_shader_binding_names(names: &[&str]) -> String {
    quote_names(names, "no named bindings")
}

/// The stage mask a graphics or ray-tracing binding carries.
///
/// The two flag types are unrelated newtypes over `u32`, so the reconciliation
/// they share is generic over this rather than over either of them.
pub trait KernelShaderStageMask: Copy + PartialEq {
    /// The mask that names no stage at all, which every accumulation starts
    /// from.
    fn mask_naming_no_stages() -> Self;

    /// Every stage this mask names, spelled as the wire and the shader spell
    /// them.
    fn named_stages(self) -> Vec<&'static str>;

    /// Whether this mask names no stage at all.
    fn names_no_stage(self) -> bool;

    /// The stages this mask names that `available` does not.
    fn stages_missing_from(self, available: Self) -> Vec<&'static str>;

    /// Whether every stage `other` names is also named here.
    fn contains_every_stage_in(self, other: Self) -> bool;
}

/// Render a stage mask for an error message.
fn quote_stage_mask<Stages: KernelShaderStageMask>(stages: Stages) -> String {
    quote_shader_stage_names(&stages.named_stages())
}

/// Render a set of shader-stage names for an error message.
pub fn quote_shader_stage_names(names: &[&str]) -> String {
    quote_names(names, "no stage")
}

fn quote_names(names: &[&str], when_empty: &str) -> String {
    if names.is_empty() {
        return when_empty.to_string();
    }
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One binding reduced to the three things reconciliation compares.
///
/// Graphics and ray tracing each map their own spec and declaration types into
/// this view, which is why neither has to give up its own explicit types to
/// share the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelBindingUnderReconciliation<'a, Kind, Stages> {
    pub name: &'a str,
    pub kind: Kind,
    pub stages: Stages,
}

/// Check a caller's binding declarations against what reflection actually
/// found, by name, for a kernel kind whose bindings carry stage masks.
///
/// An empty declaration asserts nothing and the reflected shape stands alone.
/// Otherwise every declared name must exist in the shader with the kind and
/// the stage visibility the caller claimed, and every reflected name must be
/// accounted for — leaving a shader binding unmentioned is how a dispatch
/// silently binds nothing.
///
/// A declaration whose stage mask is empty asserts nothing about stages: the
/// descriptor set layout is built from reflection, so the mask is an assertion
/// rather than an input, and asserting no stage at all is not a claim.
///
/// `stages_the_kernel_was_built_from` is the union of the stages this kernel
/// actually supplied a shader module for. Naming a stage outside it is the
/// stage-mismatch case the plan puts at construction: the kernel has no module
/// that could ever read that binding, so no dispatch could make it true.
pub fn reconcile_staged_kernel_binding_declarations<Kind, Stages>(
    kernel_kind_label: &str,
    declared: &[KernelBindingUnderReconciliation<'_, Kind, Stages>],
    reflected: &[KernelBindingUnderReconciliation<'_, Kind, Stages>],
    stages_the_kernel_was_built_from: Stages,
) -> Result<()>
where
    Kind: Copy + PartialEq + std::fmt::Debug,
    Stages: KernelShaderStageMask,
{
    if declared.is_empty() {
        return Ok(());
    }
    let shader_names: Vec<&str> = reflected.iter().map(|binding| binding.name).collect();
    let shader_declares = || quote_declared_shader_binding_names(&shader_names);

    for declaration in declared {
        let absent_stages = declaration
            .stages
            .stages_missing_from(stages_the_kernel_was_built_from);
        if !absent_stages.is_empty() {
            return Err(Error::GpuError(format!(
                "{kernel_kind_label} binding `{}` is declared for {}, which this kernel has no \
                 shader module for; it was built from {}",
                declaration.name,
                quote_names(&absent_stages, "no stage"),
                quote_stage_mask(stages_the_kernel_was_built_from)
            )));
        }

        let found = reflected
            .iter()
            .find(|binding| binding.name == declaration.name)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "{kernel_kind_label} kernel declares a binding named `{}`, which this shader \
                     does not declare; the shader declares {}",
                    declaration.name,
                    shader_declares()
                ))
            })?;

        if found.kind != declaration.kind {
            return Err(Error::GpuError(format!(
                "{kernel_kind_label} binding `{}` was declared {:?} but this shader declares it \
                 {:?}",
                declaration.name, declaration.kind, found.kind
            )));
        }

        if !declaration.stages.names_no_stage()
            && !declaration.stages.contains_every_stage_in(found.stages)
        {
            return Err(Error::GpuError(format!(
                "{kernel_kind_label} binding `{}` was declared for {} but this shader also reads \
                 it from {}; a declaration may widen a binding's visibility, never narrow it \
                 below what the shaders actually read",
                declaration.name,
                quote_stage_mask(declaration.stages),
                quote_names(
                    &found.stages.stages_missing_from(declaration.stages),
                    "no stage"
                )
            )));
        }
    }

    for binding in reflected {
        if !declared
            .iter()
            .any(|declaration| declaration.name == binding.name)
        {
            return Err(Error::GpuError(format!(
                "{kernel_kind_label} kernel leaves the shader's binding `{}` undeclared; every \
                 binding the shader declares must be accounted for",
                binding.name
            )));
        }
    }
    Ok(())
}

/// Refuse a descriptor set the kernel's single-set pipeline layout has no place
/// for.
///
/// Reflection keys its result by the set number the shader decorated, so a
/// shader whose only set is set 1 reports exactly one set — a count cannot tell
/// it apart from a shader that uses set 0, and every binding in it would be
/// dropped by a walk that reads set 0 alone.
pub fn refuse_a_descriptor_set_other_than_set_0<Stage>(
    kernel_kind_label: &str,
    stage_the_spirv_fills: Option<Stage>,
    descriptor_set_numbers_the_shader_declares: impl IntoIterator<Item = u32>,
) -> Result<()>
where
    Stage: std::fmt::Debug,
{
    let declared_sets: Vec<u32> = descriptor_set_numbers_the_shader_declares
        .into_iter()
        .collect();
    if !declared_sets.iter().any(|&set| set != 0) {
        return Ok(());
    }
    Err(Error::GpuError(match stage_the_spirv_fills {
        Some(stage) => format!(
            "{kernel_kind_label}: only descriptor set 0 is supported; SPIR-V {stage:?} stage uses \
             sets {declared_sets:?}"
        ),
        None => format!(
            "{kernel_kind_label}: only descriptor set 0 is supported; SPIR-V uses sets \
             {declared_sets:?}"
        ),
    }))
}

/// Refuse a binding the shader left unnamed.
///
/// One of the three ways a name can fail to identify one binding slot. Every
/// path that merges reflection across stages runs all three, so a name resolves
/// to exactly one slot no matter which path built the kernel.
pub fn refuse_a_binding_the_shader_left_unnamed<Stage, Kind>(
    kernel_kind_label: &str,
    stage: Stage,
    binding: u32,
    kind: Kind,
    name_the_shader_spells: &str,
) -> Result<()>
where
    Stage: std::fmt::Debug,
    Kind: std::fmt::Debug,
{
    if !name_the_shader_spells.is_empty() {
        return Ok(());
    }
    Err(Error::GpuError(format!(
        "{kernel_kind_label}: SPIR-V {stage:?} stage binding {binding} ({kind:?}) carries no \
         name — its OpName decorations were stripped, and bindings are resolved by name. Compile \
         with debug info retained (`glslc -g`) so the shader's own binding names survive \
         optimization"
    )))
}

/// Refuse one binding slot two of a kernel's stages spell differently.
pub fn refuse_one_binding_slot_two_stages_spell_differently<Stage>(
    kernel_kind_label: &str,
    stage: Stage,
    binding: u32,
    name_an_earlier_stage_spelled: &str,
    name_this_stage_spells: &str,
) -> Result<()>
where
    Stage: std::fmt::Debug,
{
    if name_an_earlier_stage_spelled == name_this_stage_spells {
        return Ok(());
    }
    Err(Error::GpuError(format!(
        "{kernel_kind_label}: binding {binding} is named `{name_an_earlier_stage_spelled}` by one \
         stage and `{name_this_stage_spells}` by the {stage:?} stage; bindings are resolved by \
         name, so one slot spelled two ways cannot be bound"
    )))
}

/// Refuse one binding name that identifies two of a kernel's slots.
///
/// Takes the merged slots in slot order, which is the order the refusal names
/// the colliding pair in.
pub fn refuse_one_binding_name_that_identifies_two_slots<'a>(
    kernel_kind_label: &str,
    names_in_binding_slot_order: impl IntoIterator<Item = (u32, &'a str)>,
) -> Result<()> {
    let mut slots_already_walked: Vec<(u32, &str)> = Vec::new();
    for (binding, name) in names_in_binding_slot_order {
        if let Some(&(earlier_binding, _)) = slots_already_walked
            .iter()
            .find(|(_, earlier_name)| *earlier_name == name)
        {
            return Err(Error::GpuError(format!(
                "{kernel_kind_label}: bindings {earlier_binding} and {binding} are both named \
                 `{name}`; bindings are resolved by name, so one name on two slots cannot be bound"
            )));
        }
        slots_already_walked.push((binding, name));
    }
    Ok(())
}

/// One shader module handed to the multi-stage reflection merge, paired with
/// the pipeline stage it fills.
#[derive(Debug, Clone, Copy)]
pub struct KernelShaderStageSpirvModule<'a, Stage> {
    pub stage: Stage,
    pub spirv: &'a [u8],
}

/// One binding the multi-stage reflection merge found, carrying the shader's
/// own name for it and every stage that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelBindingDerivedFromShaderReflection<Kind, Stages> {
    pub binding: u32,
    pub kind: Kind,
    pub stages: Stages,
    pub name: String,
}

/// The push-constant range the multi-stage reflection merge found.
#[derive(Debug, Clone, Copy)]
pub struct KernelPushConstantRangeDerivedFromShaderReflection<Stages> {
    pub size: u32,
    pub stages: Stages,
}

/// Reflect every stage of a multi-stage pipeline and merge the result into one
/// binding shape plus one push-constant range, for a kernel kind whose bindings
/// carry stage masks.
///
/// Each stage's reflection is unioned: a binding two stages read is reported
/// once with both stages set. Only descriptor set 0 is supported.
///
/// Every derived binding carries the shader's own name, and the three ways a
/// name can fail to identify one binding are rejected here rather than at
/// dispatch: a blob whose `OpName` decorations were stripped, a slot two stages
/// spell differently, and one name on two slots.
///
/// `stage_flag_of` maps a stage to its one-bit mask and
/// `binding_kind_of_descriptor_type` maps a reflected descriptor type to the
/// kernel kind's own binding kind, which is everything graphics and ray tracing
/// do not share.
pub fn derive_staged_kernel_bindings_from_shader_reflection<Stage, Kind, Stages>(
    kernel_kind_label: &str,
    stage_modules: &[KernelShaderStageSpirvModule<'_, Stage>],
    stage_flag_of: impl Fn(Stage) -> Stages,
    binding_kind_of_descriptor_type: impl Fn(RDescriptorType) -> Option<Kind>,
) -> Result<(
    Vec<KernelBindingDerivedFromShaderReflection<Kind, Stages>>,
    KernelPushConstantRangeDerivedFromShaderReflection<Stages>,
)>
where
    Stage: Copy + std::fmt::Debug,
    Kind: Copy + PartialEq + std::fmt::Debug,
    Stages: KernelShaderStageMask + std::ops::BitOrAssign,
{
    let mut merged: BTreeMap<u32, (Kind, Stages, String)> = BTreeMap::new();
    let mut push_size: u32 = 0;
    let mut push_stages = Stages::mask_naming_no_stages();

    for module in stage_modules {
        let stage = module.stage;
        let stage_flag = stage_flag_of(stage);
        let reflection = Reflection::new_from_spirv(module.spirv).map_err(|e| {
            Error::GpuError(format!(
                "{kernel_kind_label}: failed to reflect SPIR-V for {stage:?} stage: {e:?}"
            ))
        })?;
        let sets = reflection.get_descriptor_sets().map_err(|e| {
            Error::GpuError(format!(
                "{kernel_kind_label}: failed to extract descriptor sets for {stage:?} stage: {e:?}"
            ))
        })?;
        refuse_a_descriptor_set_other_than_set_0(
            kernel_kind_label,
            Some(stage),
            sets.keys().copied(),
        )?;
        if let Some(set0) = sets.get(&0) {
            for (&binding, info) in set0 {
                let kind = binding_kind_of_descriptor_type(info.ty).ok_or_else(|| {
                    Error::GpuError(format!(
                        "{kernel_kind_label}: SPIR-V {stage:?} stage binding {binding} has \
                         unsupported descriptor type {:?}",
                        info.ty
                    ))
                })?;
                refuse_a_binding_the_shader_left_unnamed(
                    kernel_kind_label,
                    stage,
                    binding,
                    kind,
                    &info.name,
                )?;
                let entry = merged.entry(binding).or_insert((
                    kind,
                    Stages::mask_naming_no_stages(),
                    info.name.clone(),
                ));
                if entry.0 != kind {
                    return Err(Error::GpuError(format!(
                        "{kernel_kind_label}: binding {binding} kind conflict — {:?} vs {kind:?} \
                         (introduced by {stage:?})",
                        entry.0
                    )));
                }
                refuse_one_binding_slot_two_stages_spell_differently(
                    kernel_kind_label,
                    stage,
                    binding,
                    &entry.2,
                    &info.name,
                )?;
                entry.1 |= stage_flag;
            }
        }
        if let Some(info) = reflection.get_push_constant_range().map_err(|e| {
            Error::GpuError(format!(
                "{kernel_kind_label}: failed to read push-constant range for {stage:?} stage: \
                 {e:?}"
            ))
        })? {
            // Vulkan permits a push-constant block to span multiple stages
            // with overlapping ranges; we report the maximum size touched
            // by any stage and the union of stages.
            push_size = push_size.max(info.size);
            push_stages |= stage_flag;
        }
    }

    let bindings: Vec<KernelBindingDerivedFromShaderReflection<Kind, Stages>> = merged
        .into_iter()
        .map(
            |(binding, (kind, stages, name))| KernelBindingDerivedFromShaderReflection {
                binding,
                kind,
                stages,
                name,
            },
        )
        .collect();
    refuse_one_binding_name_that_identifies_two_slots(
        kernel_kind_label,
        bindings
            .iter()
            .map(|derived| (derived.binding, derived.name.as_str())),
    )?;
    Ok((
        bindings,
        KernelPushConstantRangeDerivedFromShaderReflection {
            size: push_size,
            stages: push_stages,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestStageMask(u32);

    impl TestStageMask {
        const NONE: Self = Self(0);
        const VERTEX: Self = Self(0b01);
        const FRAGMENT: Self = Self(0b10);
        const VERTEX_FRAGMENT: Self = Self(0b11);
    }

    impl KernelShaderStageMask for TestStageMask {
        fn mask_naming_no_stages() -> Self {
            Self::NONE
        }

        fn named_stages(self) -> Vec<&'static str> {
            let mut names = Vec::new();
            if self.0 & 0b01 != 0 {
                names.push("vertex");
            }
            if self.0 & 0b10 != 0 {
                names.push("fragment");
            }
            names
        }

        fn names_no_stage(self) -> bool {
            self.0 == 0
        }

        fn stages_missing_from(self, available: Self) -> Vec<&'static str> {
            Self(self.0 & !available.0).named_stages()
        }

        fn contains_every_stage_in(self, other: Self) -> bool {
            (self.0 & other.0) == other.0
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestKind {
        SampledTexture,
        StorageImage,
    }

    fn binding(
        name: &str,
        kind: TestKind,
        stages: TestStageMask,
    ) -> KernelBindingUnderReconciliation<'_, TestKind, TestStageMask> {
        KernelBindingUnderReconciliation { name, kind, stages }
    }

    fn reconcile(
        declared: &[KernelBindingUnderReconciliation<'_, TestKind, TestStageMask>],
        reflected: &[KernelBindingUnderReconciliation<'_, TestKind, TestStageMask>],
        built_from: TestStageMask,
    ) -> Result<()> {
        reconcile_staged_kernel_binding_declarations("test", declared, reflected, built_from)
    }

    #[test]
    fn an_empty_declaration_lets_the_reflected_shape_stand_alone() {
        let reflected = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        reconcile(&[], &reflected, TestStageMask::VERTEX_FRAGMENT).expect("nothing asserted");
    }

    #[test]
    fn a_matching_declaration_is_accepted() {
        let bindings = [
            binding(
                "source_image",
                TestKind::SampledTexture,
                TestStageMask::FRAGMENT,
            ),
            binding(
                "output_image",
                TestKind::StorageImage,
                TestStageMask::VERTEX,
            ),
        ];
        reconcile(&bindings, &bindings, TestStageMask::VERTEX_FRAGMENT).expect("agreed");
    }

    #[test]
    fn a_name_the_shader_does_not_declare_is_refused_naming_what_it_does() {
        let declared = [binding(
            "sharpen_amount",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        let reflected = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        let refusal = reconcile(&declared, &reflected, TestStageMask::VERTEX_FRAGMENT)
            .expect_err("unknown name");
        let message = refusal.to_string();
        assert!(message.contains("sharpen_amount"), "{message}");
        assert!(message.contains("`source_image`"), "{message}");
    }

    #[test]
    fn a_kind_the_shader_disagrees_with_is_refused() {
        let declared = [binding(
            "source_image",
            TestKind::StorageImage,
            TestStageMask::FRAGMENT,
        )];
        let reflected = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        let refusal =
            reconcile(&declared, &reflected, TestStageMask::VERTEX_FRAGMENT).expect_err("kind");
        let message = refusal.to_string();
        assert!(message.contains("StorageImage"), "{message}");
        assert!(message.contains("SampledTexture"), "{message}");
    }

    #[test]
    fn a_stage_the_kernel_has_no_module_for_is_refused_at_reconciliation() {
        let declared = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::VERTEX_FRAGMENT,
        )];
        let reflected = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        let refusal =
            reconcile(&declared, &reflected, TestStageMask::FRAGMENT).expect_err("no such stage");
        let message = refusal.to_string();
        assert!(message.contains("no shader module for"), "{message}");
        assert!(message.contains("`vertex`"), "{message}");
    }

    #[test]
    fn a_declaration_narrower_than_what_the_shaders_read_is_refused() {
        let declared = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::VERTEX,
        )];
        let reflected = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::VERTEX_FRAGMENT,
        )];
        let refusal = reconcile(&declared, &reflected, TestStageMask::VERTEX_FRAGMENT)
            .expect_err("stage mismatch");
        let message = refusal.to_string();
        assert!(message.contains("declared for `vertex`"), "{message}");
        assert!(
            message.contains("also reads it from `fragment`"),
            "{message}"
        );
    }

    #[test]
    fn a_declaration_wider_than_what_the_shaders_read_is_accepted() {
        let declared = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::VERTEX_FRAGMENT,
        )];
        let reflected = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        reconcile(&declared, &reflected, TestStageMask::VERTEX_FRAGMENT)
            .expect("widening visibility is the caller's to do");
    }

    #[test]
    fn an_empty_stage_mask_asserts_nothing_about_stages() {
        let declared = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::NONE,
        )];
        let reflected = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        reconcile(&declared, &reflected, TestStageMask::VERTEX_FRAGMENT)
            .expect("no stage claim to disagree with");
    }

    #[test]
    fn leaving_a_shader_binding_undeclared_is_refused() {
        let declared = [binding(
            "source_image",
            TestKind::SampledTexture,
            TestStageMask::FRAGMENT,
        )];
        let reflected = [
            binding(
                "source_image",
                TestKind::SampledTexture,
                TestStageMask::FRAGMENT,
            ),
            binding(
                "output_image",
                TestKind::StorageImage,
                TestStageMask::FRAGMENT,
            ),
        ];
        let refusal =
            reconcile(&declared, &reflected, TestStageMask::VERTEX_FRAGMENT).expect_err("omitted");
        assert!(refusal.to_string().contains("output_image"), "{refusal}");
    }
}
