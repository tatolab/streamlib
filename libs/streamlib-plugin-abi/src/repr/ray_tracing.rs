// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Ray-tracing-pipeline `#[repr(C)]` descriptor mirrors.

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::RayTracingShaderStage`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingShaderStageRepr {
    RayGen = 0,
    Miss = 1,
    ClosestHit = 2,
    AnyHit = 3,
    Intersection = 4,
    Callable = 5,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingStage`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingStageRepr {
    /// `RayTracingShaderStageRepr` discriminant.
    pub stage: u32,
    pub _reserved_padding: u32,
    pub spv_ptr: *const u8,
    pub spv_len: usize,
    pub entry_point_ptr: *const u8,
    pub entry_point_len: usize,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::RayTracingBindingKind`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingBindingKindRepr {
    StorageBuffer = 0,
    UniformBuffer = 1,
    SampledTexture = 2,
    StorageImage = 3,
    AccelerationStructure = 4,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingBindingSpec`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingBindingSpecRepr {
    pub binding: u32,
    /// `RayTracingBindingKindRepr` discriminant.
    pub kind: u32,
    /// `RayTracingShaderStageFlags::bits()`.
    pub stages: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingPushConstants`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingPushConstantsRepr {
    pub size: u32,
    /// `RayTracingShaderStageFlags::bits()`.
    pub stages: u32,
}

/// Discriminant for the [`RayTracingShaderGroupRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingShaderGroupKindRepr {
    General = 0,
    TrianglesHit = 1,
    ProceduralHit = 2,
}

/// Sentinel value for `Option<u32>` stage references; matches
/// `VK_SHADER_UNUSED_KHR == ~0u`. Reserved by the Vulkan spec and
/// never a valid in-range stage index.
pub const RAY_TRACING_SHADER_UNUSED: u32 = u32::MAX;

/// Tagged-union mirror of `streamlib::core::rhi::RayTracingShaderGroup`.
///
/// Field interpretation per `kind`:
/// - `General`: `general_or_intersection` carries the general stage
///   index; `closest_hit` / `any_hit` are [`RAY_TRACING_SHADER_UNUSED`].
/// - `TrianglesHit`: `general_or_intersection` is
///   [`RAY_TRACING_SHADER_UNUSED`]; `closest_hit` / `any_hit` carry the
///   shader indices ([`RAY_TRACING_SHADER_UNUSED`] = `None`).
/// - `ProceduralHit`: `general_or_intersection` carries the
///   intersection stage index; `closest_hit` / `any_hit` carry the
///   optional shader indices.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingShaderGroupRepr {
    /// `RayTracingShaderGroupKindRepr` discriminant.
    pub kind: u32,
    /// General stage (General) / intersection stage (ProceduralHit).
    pub general_or_intersection: u32,
    /// Closest-hit stage. [`RAY_TRACING_SHADER_UNUSED`] = absent.
    pub closest_hit: u32,
    /// Any-hit stage. [`RAY_TRACING_SHADER_UNUSED`] = absent.
    pub any_hit: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingKernelDescriptor`.
///
/// All pointer fields borrow into caller-owned memory and must
/// remain valid for the duration of the vtable call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingKernelDescriptorRepr {
    pub label_ptr: *const u8,
    pub label_len: usize,
    pub stages_ptr: *const RayTracingStageRepr,
    pub stages_len: usize,
    pub groups_ptr: *const RayTracingShaderGroupRepr,
    pub groups_len: usize,
    pub bindings_ptr: *const RayTracingBindingSpecRepr,
    pub bindings_len: usize,
    pub push_constants: RayTracingPushConstantsRepr,
    pub max_recursion_depth: u32,
    pub _reserved_padding: u32,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn ray_tracing_stage_repr_layout() {
        assert_eq!(size_of::<RayTracingStageRepr>(), 40);
        assert_eq!(align_of::<RayTracingStageRepr>(), 8);
        assert_eq!(offset_of!(RayTracingStageRepr, stage), 0);
        assert_eq!(offset_of!(RayTracingStageRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(RayTracingStageRepr, spv_ptr), 8);
        assert_eq!(offset_of!(RayTracingStageRepr, spv_len), 16);
        assert_eq!(offset_of!(RayTracingStageRepr, entry_point_ptr), 24);
        assert_eq!(offset_of!(RayTracingStageRepr, entry_point_len), 32);
    }

    #[test]
    fn ray_tracing_binding_spec_repr_layout() {
        assert_eq!(size_of::<RayTracingBindingSpecRepr>(), 16);
        assert_eq!(align_of::<RayTracingBindingSpecRepr>(), 4);
        assert_eq!(offset_of!(RayTracingBindingSpecRepr, binding), 0);
        assert_eq!(offset_of!(RayTracingBindingSpecRepr, kind), 4);
        assert_eq!(offset_of!(RayTracingBindingSpecRepr, stages), 8);
        assert_eq!(
            offset_of!(RayTracingBindingSpecRepr, _reserved_padding),
            12
        );
    }

    #[test]
    fn ray_tracing_push_constants_repr_layout() {
        assert_eq!(size_of::<RayTracingPushConstantsRepr>(), 8);
        assert_eq!(align_of::<RayTracingPushConstantsRepr>(), 4);
        assert_eq!(offset_of!(RayTracingPushConstantsRepr, size), 0);
        assert_eq!(offset_of!(RayTracingPushConstantsRepr, stages), 4);
    }

    #[test]
    fn ray_tracing_shader_group_repr_layout() {
        assert_eq!(size_of::<RayTracingShaderGroupRepr>(), 16);
        assert_eq!(align_of::<RayTracingShaderGroupRepr>(), 4);
        assert_eq!(offset_of!(RayTracingShaderGroupRepr, kind), 0);
        assert_eq!(
            offset_of!(RayTracingShaderGroupRepr, general_or_intersection),
            4
        );
        assert_eq!(offset_of!(RayTracingShaderGroupRepr, closest_hit), 8);
        assert_eq!(offset_of!(RayTracingShaderGroupRepr, any_hit), 12);
    }

    #[test]
    fn ray_tracing_kernel_descriptor_repr_layout() {
        // 4 (ptr,len) pairs (64) + push_constants(8) +
        // max_recursion_depth(4) + pad(4) = 80 bytes.
        assert_eq!(size_of::<RayTracingKernelDescriptorRepr>(), 80);
        assert_eq!(align_of::<RayTracingKernelDescriptorRepr>(), 8);
        assert_eq!(offset_of!(RayTracingKernelDescriptorRepr, label_ptr), 0);
        assert_eq!(offset_of!(RayTracingKernelDescriptorRepr, label_len), 8);
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, stages_ptr),
            16
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, stages_len),
            24
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, groups_ptr),
            32
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, groups_len),
            40
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, bindings_ptr),
            48
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, bindings_len),
            56
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, push_constants),
            64
        );
        assert_eq!(
            offset_of!(
                RayTracingKernelDescriptorRepr,
                max_recursion_depth
            ),
            72
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, _reserved_padding),
            76
        );
    }

    #[test]
    fn ray_tracing_shader_unused_sentinel() {
        // The "absent stage" sentinel matches VK_SHADER_UNUSED_KHR.
        assert_eq!(RAY_TRACING_SHADER_UNUSED, u32::MAX);
    }
}
