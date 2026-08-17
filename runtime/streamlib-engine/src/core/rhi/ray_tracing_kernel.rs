// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor types for the ray-tracing-kernel RHI abstraction.
//!
//! Mirrors [`super::compute_kernel`] and [`super::graphics_kernel`] for
//! ray-tracing-pipeline work — RayGen / Miss / ClosestHit / AnyHit /
//! Intersection / Callable stages plus shader-binding-table machinery.
//!
//! Pattern matches `VulkanComputeKernel`: declare the binding shape,
//! shader-group layout, and push-constant range once as data; the RHI
//! reflects every stage's SPIR-V at kernel creation, validates the
//! declarations, and from that point on the caller binds resources by
//! slot via simple typed setters.

use std::borrow::Cow;

use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};

use crate::core::{Error, Result};

use super::kernel_binding_names::{
    KernelBindingUnderReconciliation, KernelShaderStageMask,
    reconcile_staged_kernel_binding_declarations,
};

/// Shader stages that contribute to a ray-tracing pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RayTracingShaderStage {
    RayGen,
    Miss,
    ClosestHit,
    AnyHit,
    Intersection,
    Callable,
}

/// Bitflags for which ray-tracing stages a binding or push-constant range
/// is visible to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RayTracingShaderStageFlags(u32);

impl RayTracingShaderStageFlags {
    pub const NONE: Self = Self(0);
    pub const RAYGEN: Self = Self(0b00_0001);
    pub const MISS: Self = Self(0b00_0010);
    pub const CLOSEST_HIT: Self = Self(0b00_0100);
    pub const ANY_HIT: Self = Self(0b00_1000);
    pub const INTERSECTION: Self = Self(0b01_0000);
    pub const CALLABLE: Self = Self(0b10_0000);
    pub const ALL_HIT: Self = Self(0b00_1100);
    pub const ALL: Self = Self(0b11_1111);

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The mask a raw bitmask names, or `None` if it names a bit no
    /// ray-tracing stage owns.
    ///
    /// An unknown bit is refused rather than masked off: a caller that set it
    /// meant a stage, and dropping it silently would make the declaration
    /// assert less than it said.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::ALL.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }
}

impl std::ops::BitOr for RayTracingShaderStageFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for RayTracingShaderStageFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// One stage of a ray-tracing pipeline: SPIR-V blob plus stage classification.
#[derive(Debug, Clone, Copy)]
pub struct RayTracingStage<'a> {
    pub stage: RayTracingShaderStage,
    pub spv: &'a [u8],
    /// Entry point name. Defaults to `"main"`.
    pub entry_point: &'a str,
}

impl<'a> RayTracingStage<'a> {
    pub const fn ray_gen(spv: &'a [u8]) -> Self {
        Self {
            stage: RayTracingShaderStage::RayGen,
            spv,
            entry_point: "main",
        }
    }

    pub const fn miss(spv: &'a [u8]) -> Self {
        Self {
            stage: RayTracingShaderStage::Miss,
            spv,
            entry_point: "main",
        }
    }

    pub const fn closest_hit(spv: &'a [u8]) -> Self {
        Self {
            stage: RayTracingShaderStage::ClosestHit,
            spv,
            entry_point: "main",
        }
    }

    pub const fn any_hit(spv: &'a [u8]) -> Self {
        Self {
            stage: RayTracingShaderStage::AnyHit,
            spv,
            entry_point: "main",
        }
    }

    pub const fn intersection(spv: &'a [u8]) -> Self {
        Self {
            stage: RayTracingShaderStage::Intersection,
            spv,
            entry_point: "main",
        }
    }

    pub const fn callable(spv: &'a [u8]) -> Self {
        Self {
            stage: RayTracingShaderStage::Callable,
            spv,
            entry_point: "main",
        }
    }
}

/// Resource kind bound at a particular slot in a ray-tracing kernel's
/// descriptor set 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingBindingKind {
    StorageBuffer,
    UniformBuffer,
    SampledTexture,
    StorageImage,
    /// Top-level acceleration structure (`VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR`).
    /// Bound via [`super::super::vulkan::rhi::VulkanAccelerationStructure`]'s top-level
    /// handle; the descriptor write chains
    /// `VkWriteDescriptorSetAccelerationStructureKHR` rather than a buffer or
    /// image info.
    AccelerationStructure,
}

/// One binding declaration: (binding index, resource kind, visible stages, the
/// shader's own name for the binding).
///
/// `name` is `None` on a declaration that asserts only slot, kind and stages,
/// and `Some` on every spec the RHI derived from reflection: the numeric
/// binding is what the descriptor set is built from, the name is what a
/// by-name dispatch resolves against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RayTracingBindingSpec {
    pub binding: u32,
    pub kind: RayTracingBindingKind,
    pub stages: RayTracingShaderStageFlags,
    pub name: Option<Cow<'static, str>>,
}

/// What a caller asserts about one ray-tracing binding before reflection has
/// assigned it a slot: the shader's name for it, the kind expected there, and
/// the stages the caller believes read it.
///
/// Deliberately not a [`RayTracingBindingSpec`]: a declaration has no slot, and
/// carrying a placeholder slot in a spec would put a meaningless number in a
/// field every resolved path treats as load-bearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RayTracingBindingDeclaration {
    pub name: String,
    pub kind: RayTracingBindingKind,
    pub stages: RayTracingShaderStageFlags,
}

impl KernelShaderStageMask for RayTracingShaderStageFlags {
    fn named_stages(self) -> Vec<&'static str> {
        [
            (Self::RAYGEN, "ray_gen"),
            (Self::MISS, "miss"),
            (Self::CLOSEST_HIT, "closest_hit"),
            (Self::ANY_HIT, "any_hit"),
            (Self::INTERSECTION, "intersection"),
            (Self::CALLABLE, "callable"),
        ]
        .into_iter()
        .filter(|(flag, _)| self.contains(*flag))
        .map(|(_, name)| name)
        .collect()
    }

    fn names_no_stage(self) -> bool {
        self == Self::NONE
    }

    fn stages_missing_from(self, available: Self) -> Vec<&'static str> {
        Self(self.0 & !available.0).named_stages()
    }

    fn contains_every_stage_in(self, other: Self) -> bool {
        self.contains(other)
    }
}

/// Check a caller's ray-tracing binding declarations against what the stages'
/// reflection actually found, by name.
///
/// Runs at kernel construction, where the multi-stage declaration is built —
/// which is the only place a stage mismatch can be caught, because a dispatch
/// never revisits which stage reads what. A ray-tracing kernel's stage set
/// varies per kernel, so naming `any_hit` on a kernel built without an any-hit
/// module is a mistake no dispatch could ever make true.
pub(crate) fn reconcile_ray_tracing_binding_declarations(
    declared: &[RayTracingBindingDeclaration],
    reflected: &[RayTracingBindingSpec],
    stages_the_kernel_was_built_from: RayTracingShaderStageFlags,
) -> Result<()> {
    let declared_view: Vec<_> = declared
        .iter()
        .map(|declaration| KernelBindingUnderReconciliation {
            name: declaration.name.as_str(),
            kind: declaration.kind,
            stages: declaration.stages,
        })
        .collect();
    let reflected_view: Vec<_> = reflected
        .iter()
        .filter_map(|spec| {
            Some(KernelBindingUnderReconciliation {
                name: spec.name.as_deref()?,
                kind: spec.kind,
                stages: spec.stages,
            })
        })
        .collect();
    reconcile_staged_kernel_binding_declarations(
        "ray-tracing",
        &declared_view,
        &reflected_view,
        stages_the_kernel_was_built_from,
    )
}

/// Reflect every stage of a ray-tracing pipeline and return the merged binding
/// shape + total push-constant size + visible stages per binding.
///
/// The ray-tracing twin of
/// [`super::graphics_kernel::derive_bindings_from_spirv_multistage`], and the
/// same contract: each stage's reflection is unioned, every derived spec
/// carries the shader's own name, and the two ways a name can fail to identify
/// one binding — a slot two stages spell differently, and one name on two
/// slots — are rejected here rather than at dispatch. A blob whose `OpName`
/// decorations were stripped is rejected for the same reason.
pub fn derive_ray_tracing_bindings_from_spirv_multistage(
    stages: &[RayTracingStage<'_>],
) -> Result<(Vec<RayTracingBindingSpec>, RayTracingPushConstants)> {
    let mut merged: std::collections::BTreeMap<
        u32,
        (RayTracingBindingKind, RayTracingShaderStageFlags, String),
    > = std::collections::BTreeMap::new();
    let mut push_size: u32 = 0;
    let mut push_stages = RayTracingShaderStageFlags::NONE;

    for stage in stages {
        let stage_flag = ray_tracing_stage_to_flag(stage.stage);
        let reflection = Reflection::new_from_spirv(stage.spv).map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel: failed to reflect SPIR-V for {:?} stage: {e:?}",
                stage.stage
            ))
        })?;
        let sets = reflection.get_descriptor_sets().map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel: failed to extract descriptor sets for {:?} stage: {e:?}",
                stage.stage
            ))
        })?;
        if sets.len() > 1 {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel: only descriptor set 0 is supported; SPIR-V {:?} stage uses \
                 sets {:?}",
                stage.stage,
                sets.keys().collect::<Vec<_>>()
            )));
        }
        if let Some(set0) = sets.get(&0) {
            for (&binding, info) in set0 {
                let kind = ray_tracing_spirv_type_to_kind(info.ty).ok_or_else(|| {
                    Error::GpuError(format!(
                        "Ray-tracing kernel: SPIR-V {:?} stage binding {binding} has unsupported \
                         descriptor type {:?}",
                        stage.stage, info.ty
                    ))
                })?;
                if info.name.is_empty() {
                    return Err(Error::GpuError(format!(
                        "Ray-tracing kernel: SPIR-V {:?} stage binding {binding} ({kind:?}) \
                         carries no name — its OpName decorations were stripped, and bindings are \
                         resolved by name. Compile with debug info retained (`glslc -g`) so the \
                         shader's own binding names survive optimization",
                        stage.stage
                    )));
                }
                let entry = merged.entry(binding).or_insert((
                    kind,
                    RayTracingShaderStageFlags::NONE,
                    info.name.clone(),
                ));
                if entry.0 != kind {
                    return Err(Error::GpuError(format!(
                        "Ray-tracing kernel: binding {binding} kind conflict — {:?} vs {:?} \
                         (introduced by {:?})",
                        entry.0, kind, stage.stage
                    )));
                }
                if entry.2 != info.name {
                    return Err(Error::GpuError(format!(
                        "Ray-tracing kernel: binding {binding} is named `{}` by one stage and \
                         `{}` by the {:?} stage; bindings are resolved by name, so one slot \
                         spelled two ways cannot be bound",
                        entry.2, info.name, stage.stage
                    )));
                }
                entry.1 |= stage_flag;
            }
        }
        if let Some(info) = reflection.get_push_constant_range().map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel: failed to read push-constant range for {:?} stage: {e:?}",
                stage.stage
            ))
        })? {
            push_size = push_size.max(info.size);
            push_stages |= stage_flag;
        }
    }

    let bindings: Vec<RayTracingBindingSpec> = merged
        .into_iter()
        .map(|(binding, (kind, stages, name))| RayTracingBindingSpec {
            binding,
            kind,
            stages,
            name: Some(Cow::Owned(name)),
        })
        .collect();
    for (index, spec) in bindings.iter().enumerate() {
        if let Some(earlier) = bindings[..index]
            .iter()
            .find(|earlier| earlier.name == spec.name)
        {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel: bindings {} and {} are both named `{}`; bindings are \
                 resolved by name, so one name on two slots cannot be bound",
                earlier.binding,
                spec.binding,
                spec.name.as_deref().unwrap_or_default()
            )));
        }
    }
    Ok((
        bindings,
        RayTracingPushConstants {
            size: push_size,
            stages: push_stages,
        },
    ))
}

/// The union of the stages a set of shader modules covers.
pub fn ray_tracing_stages_covered_by(stages: &[RayTracingStage<'_>]) -> RayTracingShaderStageFlags {
    stages
        .iter()
        .fold(RayTracingShaderStageFlags::NONE, |covered, stage| {
            covered | ray_tracing_stage_to_flag(stage.stage)
        })
}

const fn ray_tracing_stage_to_flag(stage: RayTracingShaderStage) -> RayTracingShaderStageFlags {
    match stage {
        RayTracingShaderStage::RayGen => RayTracingShaderStageFlags::RAYGEN,
        RayTracingShaderStage::Miss => RayTracingShaderStageFlags::MISS,
        RayTracingShaderStage::ClosestHit => RayTracingShaderStageFlags::CLOSEST_HIT,
        RayTracingShaderStage::AnyHit => RayTracingShaderStageFlags::ANY_HIT,
        RayTracingShaderStage::Intersection => RayTracingShaderStageFlags::INTERSECTION,
        RayTracingShaderStage::Callable => RayTracingShaderStageFlags::CALLABLE,
    }
}

/// The binding kind a reflected descriptor type names, or `None` for a type no
/// ray-tracing binding kind covers.
pub(crate) fn ray_tracing_spirv_type_to_kind(
    ty: RDescriptorType,
) -> Option<RayTracingBindingKind> {
    match ty {
        RDescriptorType::STORAGE_BUFFER => Some(RayTracingBindingKind::StorageBuffer),
        RDescriptorType::UNIFORM_BUFFER => Some(RayTracingBindingKind::UniformBuffer),
        RDescriptorType::COMBINED_IMAGE_SAMPLER => Some(RayTracingBindingKind::SampledTexture),
        RDescriptorType::STORAGE_IMAGE => Some(RayTracingBindingKind::StorageImage),
        RDescriptorType::ACCELERATION_STRUCTURE_KHR => {
            Some(RayTracingBindingKind::AccelerationStructure)
        }
        _ => None,
    }
}

impl RayTracingBindingSpec {
    pub const fn storage_buffer(binding: u32, stages: RayTracingShaderStageFlags) -> Self {
        Self {
            binding,
            kind: RayTracingBindingKind::StorageBuffer,
            stages,
            name: None,
        }
    }

    pub const fn uniform_buffer(binding: u32, stages: RayTracingShaderStageFlags) -> Self {
        Self {
            binding,
            kind: RayTracingBindingKind::UniformBuffer,
            stages,
            name: None,
        }
    }

    pub const fn sampled_texture(binding: u32, stages: RayTracingShaderStageFlags) -> Self {
        Self {
            binding,
            kind: RayTracingBindingKind::SampledTexture,
            stages,
            name: None,
        }
    }

    pub const fn storage_image(binding: u32, stages: RayTracingShaderStageFlags) -> Self {
        Self {
            binding,
            kind: RayTracingBindingKind::StorageImage,
            stages,
            name: None,
        }
    }

    pub const fn acceleration_structure(binding: u32, stages: RayTracingShaderStageFlags) -> Self {
        Self {
            binding,
            kind: RayTracingBindingKind::AccelerationStructure,
            stages,
            name: None,
        }
    }

    /// Assert the shader's spelling of this binding as well as its slot.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Push-constant range declaration. Set `size = 0` to opt out.
#[derive(Debug, Clone, Copy)]
pub struct RayTracingPushConstants {
    pub size: u32,
    pub stages: RayTracingShaderStageFlags,
}

impl RayTracingPushConstants {
    pub const NONE: Self = Self {
        size: 0,
        stages: RayTracingShaderStageFlags::NONE,
    };
}

/// One shader group entry in the ray-tracing pipeline. Each variant maps to
/// one entry in the SBT — the kind plus the stage indices it composes.
///
/// Stage indices refer to positions in [`RayTracingKernelDescriptor::stages`].
#[derive(Debug, Clone, Copy)]
pub enum RayTracingShaderGroup {
    /// General group: contributes one ray-gen, miss, or callable stage.
    General {
        /// Index into [`RayTracingKernelDescriptor::stages`].
        general: u32,
    },
    /// Triangle hit group: closest-hit and/or any-hit shader against
    /// the built-in triangle intersection test.
    TrianglesHit {
        closest_hit: Option<u32>,
        any_hit: Option<u32>,
    },
    /// Procedural hit group: a custom intersection shader plus optional
    /// closest-hit and any-hit shaders. The intersection shader runs
    /// against AABB primitives in the BLAS.
    ProceduralHit {
        intersection: u32,
        closest_hit: Option<u32>,
        any_hit: Option<u32>,
    },
}

/// Ray-tracing kernel descriptor: shader stages + shader-group layout +
/// binding declarations + push-constant range.
///
/// Pass to `GpuContext::create_ray_tracing_kernel` (or
/// `VulkanRayTracingKernel::new`). Reflection at kernel creation validates
/// every stage's SPIR-V matches the declared bindings and push-constant
/// size; mismatches abort kernel construction loudly.
#[derive(Debug, Clone)]
pub struct RayTracingKernelDescriptor<'a> {
    /// Human-readable label used in error messages and tracing.
    pub label: &'a str,
    /// Shader stages composing the pipeline. Indices into this slice are
    /// referenced by [`Self::groups`].
    pub stages: &'a [RayTracingStage<'a>],
    /// Shader-group layout. The order here is the order entries appear in
    /// the SBT regions (raygen / miss / hit / callable).
    pub groups: &'a [RayTracingShaderGroup],
    /// Binding declarations for descriptor set 0.
    pub bindings: &'a [RayTracingBindingSpec],
    /// Push-constant range. Use [`RayTracingPushConstants::NONE`] when the
    /// shaders use no push constants.
    pub push_constants: RayTracingPushConstants,
    /// Maximum ray recursion depth. Must be ≤ device's
    /// `maxRayRecursionDepth` from
    /// `VkPhysicalDeviceRayTracingPipelinePropertiesKHR`.
    pub max_recursion_depth: u32,
}

/// Validate that group references resolve to in-range stage indices and
/// that the kind/stage agreement holds (e.g. `General::general` must point
/// to a RayGen, Miss, or Callable; triangle/procedural hit groups must
/// reference ClosestHit / AnyHit / Intersection stages).
///
/// Public so the kernel implementation and any future escalate-IPC handler
/// share the same checks.
pub fn validate_shader_groups(
    label: &str,
    stages: &[RayTracingStage<'_>],
    groups: &[RayTracingShaderGroup],
) -> Result<()> {
    if groups.is_empty() {
        return Err(Error::GpuError(format!(
            "Ray-tracing kernel '{label}': at least one shader group is required"
        )));
    }
    for (group_idx, group) in groups.iter().enumerate() {
        match *group {
            RayTracingShaderGroup::General { general } => {
                let stage = stage_at(label, stages, "General::general", group_idx, general)?;
                match stage {
                    RayTracingShaderStage::RayGen
                    | RayTracingShaderStage::Miss
                    | RayTracingShaderStage::Callable => {}
                    other => {
                        return Err(Error::GpuError(format!(
                            "Ray-tracing kernel '{label}': group {group_idx} declares General with stage index pointing to {other:?}; must be RayGen / Miss / Callable"
                        )));
                    }
                }
            }
            RayTracingShaderGroup::TrianglesHit {
                closest_hit,
                any_hit,
            } => {
                if closest_hit.is_none() && any_hit.is_none() {
                    return Err(Error::GpuError(format!(
                        "Ray-tracing kernel '{label}': group {group_idx} TrianglesHit must set at least one of closest_hit / any_hit"
                    )));
                }
                if let Some(idx) = closest_hit {
                    expect_stage(
                        label,
                        stages,
                        "TrianglesHit::closest_hit",
                        group_idx,
                        idx,
                        RayTracingShaderStage::ClosestHit,
                    )?;
                }
                if let Some(idx) = any_hit {
                    expect_stage(
                        label,
                        stages,
                        "TrianglesHit::any_hit",
                        group_idx,
                        idx,
                        RayTracingShaderStage::AnyHit,
                    )?;
                }
            }
            RayTracingShaderGroup::ProceduralHit {
                intersection,
                closest_hit,
                any_hit,
            } => {
                expect_stage(
                    label,
                    stages,
                    "ProceduralHit::intersection",
                    group_idx,
                    intersection,
                    RayTracingShaderStage::Intersection,
                )?;
                if let Some(idx) = closest_hit {
                    expect_stage(
                        label,
                        stages,
                        "ProceduralHit::closest_hit",
                        group_idx,
                        idx,
                        RayTracingShaderStage::ClosestHit,
                    )?;
                }
                if let Some(idx) = any_hit {
                    expect_stage(
                        label,
                        stages,
                        "ProceduralHit::any_hit",
                        group_idx,
                        idx,
                        RayTracingShaderStage::AnyHit,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn stage_at(
    label: &str,
    stages: &[RayTracingStage<'_>],
    field: &str,
    group_idx: usize,
    stage_idx: u32,
) -> Result<RayTracingShaderStage> {
    stages
        .get(stage_idx as usize)
        .map(|s| s.stage)
        .ok_or_else(|| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{label}': group {group_idx} field {field} references stage {stage_idx}, but only {} stages were declared",
                stages.len()
            ))
        })
}

fn expect_stage(
    label: &str,
    stages: &[RayTracingStage<'_>],
    field: &str,
    group_idx: usize,
    stage_idx: u32,
    expected: RayTracingShaderStage,
) -> Result<()> {
    let actual = stage_at(label, stages, field, group_idx, stage_idx)?;
    if actual != expected {
        return Err(Error::GpuError(format!(
            "Ray-tracing kernel '{label}': group {group_idx} field {field} references stage {stage_idx} ({actual:?}); expected {expected:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_stage(stage: RayTracingShaderStage) -> RayTracingStage<'static> {
        RayTracingStage {
            stage,
            spv: &[],
            entry_point: "main",
        }
    }

    #[test]
    fn shader_stage_flags_compose_via_bitor() {
        let combined =
            RayTracingShaderStageFlags::CLOSEST_HIT | RayTracingShaderStageFlags::ANY_HIT;
        assert!(combined.contains(RayTracingShaderStageFlags::CLOSEST_HIT));
        assert!(combined.contains(RayTracingShaderStageFlags::ANY_HIT));
        assert!(!combined.contains(RayTracingShaderStageFlags::RAYGEN));
        assert_eq!(combined, RayTracingShaderStageFlags::ALL_HIT);
    }

    #[test]
    fn validate_shader_groups_accepts_minimal_pipeline() {
        let stages = [
            dummy_stage(RayTracingShaderStage::RayGen),
            dummy_stage(RayTracingShaderStage::Miss),
            dummy_stage(RayTracingShaderStage::ClosestHit),
        ];
        let groups = [
            RayTracingShaderGroup::General { general: 0 },
            RayTracingShaderGroup::General { general: 1 },
            RayTracingShaderGroup::TrianglesHit {
                closest_hit: Some(2),
                any_hit: None,
            },
        ];
        validate_shader_groups("ok", &stages, &groups).expect("valid");
    }

    #[test]
    fn validate_shader_groups_rejects_empty() {
        let stages = [dummy_stage(RayTracingShaderStage::RayGen)];
        let err = validate_shader_groups("empty", &stages, &[]).unwrap_err();
        assert!(format!("{err}").contains("at least one shader group"));
    }

    #[test]
    fn validate_shader_groups_rejects_general_pointing_to_hit_stage() {
        let stages = [
            dummy_stage(RayTracingShaderStage::RayGen),
            dummy_stage(RayTracingShaderStage::ClosestHit),
        ];
        let groups = [RayTracingShaderGroup::General { general: 1 }];
        let err = validate_shader_groups("bad-general", &stages, &groups).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("must be RayGen") && msg.contains("ClosestHit"),
            "got: {msg}"
        );
    }

    #[test]
    fn validate_shader_groups_rejects_hit_with_neither_closest_nor_any() {
        let stages = [dummy_stage(RayTracingShaderStage::RayGen)];
        let groups = [RayTracingShaderGroup::TrianglesHit {
            closest_hit: None,
            any_hit: None,
        }];
        let err = validate_shader_groups("bad-hit", &stages, &groups).unwrap_err();
        assert!(format!("{err}").contains("at least one of closest_hit / any_hit"));
    }

    #[test]
    fn validate_shader_groups_rejects_out_of_range_stage_index() {
        let stages = [dummy_stage(RayTracingShaderStage::RayGen)];
        let groups = [RayTracingShaderGroup::General { general: 5 }];
        let err = validate_shader_groups("oob", &stages, &groups).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("references stage 5") && msg.contains("only 1 stages"),
            "got: {msg}"
        );
    }
}
