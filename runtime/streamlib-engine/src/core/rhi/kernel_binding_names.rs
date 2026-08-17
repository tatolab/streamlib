// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Resolving a kernel's descriptor bindings by the shader's own names.
//!
//! Every pipeline kind reads its binding names out of SPIR-V reflection and
//! checks a caller's declaration against them rather than letting the
//! declaration replace them. Compute has no stage axis and reconciles in
//! [`super::compute_kernel`]; graphics and ray tracing scope each binding to
//! the stages that read it, and share the reconciliation here because the only
//! thing that differs between them is which two newtypes they name.

use crate::core::{Error, Result};

/// Render a shader's declared binding names for an error message.
pub fn quote_declared_shader_binding_names(names: &[&str]) -> String {
    if names.is_empty() {
        return "no named bindings".to_string();
    }
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The stage mask a graphics or ray-tracing binding carries.
///
/// The two flag types are unrelated newtypes over `u32`, so the reconciliation
/// they share is generic over this rather than over either of them.
pub trait KernelShaderStageMask: Copy + PartialEq {
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
    quote_names(&stages.named_stages(), "no stage")
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
