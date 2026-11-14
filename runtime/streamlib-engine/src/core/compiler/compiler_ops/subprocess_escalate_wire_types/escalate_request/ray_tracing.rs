// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterAccelerationStructureBlas {
    /// Index blob, lowercase hex-encoded little-endian u32s.
    /// Must be a multiple of 3 — three indices per triangle.
    /// The host decodes these into a `&[u32]` and forwards to
    /// `VulkanAccelerationStructure::build_triangles_blas`.
    pub(crate) indices_hex: String,

    /// Human-readable label the host gives the structure: it names this BLAS in
    /// RHI errors and in validation-layer messages. The returned `as_id` is a
    /// fresh UUID and derives nothing from it.
    pub(crate) label: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Vertex blob, lowercase hex-encoded little-endian f32s
    /// (`R32G32B32_SFLOAT`, stride 12 bytes — interleaved `[x,y,z,x,y,z,...]`).
    /// Length in bytes after hex decoding must be a multiple of 12; total f32
    /// count must equal `3 × vertex_count`.
    pub(crate) vertices_hex: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterAccelerationStructureTlasInstance {
    /// Handle returned by a prior `register_acceleration_structure_blas`
    /// response. Must reference a BLAS, not a TLAS — the host validates kind
    /// and rejects mismatches.
    pub(crate) blas_id: String,

    /// 24-bit user data exposed to hit shaders as `gl_InstanceCustomIndexEXT`.
    /// The high 8 bits must be zero.
    pub(crate) custom_index: u32,

    /// `VkGeometryInstanceFlagsKHR` bitmask. The host passes this through to
    /// `VkAccelerationStructureInstanceKHR` unchanged. `0` selects the spec
    /// default; conventional combinations: `1 = TRIANGLE_FACING_CULL_DISABLE`,
    /// `4 = FORCE_OPAQUE`.
    pub(crate) flags: u32,

    /// 8-bit visibility mask. Rays specify a `cullMask`; the instance is hit
    /// only when `(mask & cullMask) != 0`. The wire form
    /// is uint32 and the host rejects values > 0xff.
    pub(crate) mask: u32,

    /// Offset added to the SBT hit-group index. Usually 0 for single-hit-group
    /// RT pipelines.
    pub(crate) sbt_record_offset: u32,

    /// Row-major 3×4 affine transform applied to the BLAS geometry in world
    /// space. Exactly 12 floats — three rows of four — laid out `[m00, m01,
    /// m02, m03, m10, ..., m23]`. Matches `VkTransformMatrixKHR` directly.
    pub(crate) transform: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterAccelerationStructureTlas {
    /// One TLAS instance per entry. The host resolves each `blas_id` through
    /// `GpuContext`'s acceleration-structure registry and forwards to
    /// `VulkanAccelerationStructure::build_tlas`. Empty array is rejected (TLAS
    /// must have at least one instance).
    pub(crate) instances: Vec<EscalateRequestRegisterAccelerationStructureTlasInstance>,

    /// Human-readable label the host gives the structure: it names this TLAS in
    /// RHI errors and in validation-layer messages. The returned `as_id` is a
    /// fresh UUID and derives nothing from it.
    pub(crate) label: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}

/// Resource kind for a ray-tracing binding slot.
///
/// One enum for the register array, the dispatch array and the register
/// response — they name the same five kinds, and two spellings of one set is
/// two things to keep in lockstep forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EscalateRayTracingBindingKind {
    #[serde(rename = "acceleration_structure")]
    AccelerationStructure,

    #[serde(rename = "sampled_texture")]
    SampledTexture,

    #[serde(rename = "storage_buffer")]
    StorageBuffer,

    #[serde(rename = "storage_image")]
    StorageImage,

    #[serde(rename = "uniform_buffer")]
    UniformBuffer,
}

impl EscalateRayTracingBindingKind {
    /// This kind's spelling on the wire — what a register response hands back
    /// and the next dispatch echoes.
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::AccelerationStructure => "acceleration_structure",
            Self::SampledTexture => "sampled_texture",
            Self::StorageBuffer => "storage_buffer",
            Self::StorageImage => "storage_image",
            Self::UniformBuffer => "uniform_buffer",
        }
    }
}

/// One binding a ray-tracing kernel declares at registration, named as the
/// shaders name it. The slot number is not on the wire — it comes from
/// reflection, and the name is what a dispatch resolves against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterRayTracingKernelBinding {
    /// Resource kind the caller expects at this name. Checked against
    /// reflection at registration; a mismatch is an `err` response.
    pub(crate) kind: EscalateRayTracingBindingKind,

    /// The shaders' own name for the binding.
    pub(crate) name: String,

    /// Bitmask of RT stages the binding is visible to. Bits: `1=RAYGEN`,
    /// `2=MISS`, `4=CLOSEST_HIT`, `8=ANY_HIT`, `16=INTERSECTION`,
    /// `32=CALLABLE`. `0` asserts nothing about stages.
    ///
    /// A declaration may widen a binding's visibility past what the shaders
    /// read, never narrow it below; naming a stage this kernel has no module
    /// for is refused at registration, where the multi-stage declaration is
    /// built. A ray-tracing kernel's stage set varies per kernel, so that is
    /// the case this refusal exists for.
    pub(crate) stages: u32,
}

/// Value a shader-group's optional stage index carries when the group names no
/// stage there. Every stage-index field is always present on the wire, so
/// "absent" needs a value rather than an omission.
pub(crate) const RAY_TRACING_STAGE_INDEX_NONE: u32 = u32::MAX;

/// - `general`: contributes one ray-gen, miss, or
///   callable stage via `general_stage`.
/// - `triangles_hit`: triangle hit group; sets at least
///   one of `closest_hit_stage` / `any_hit_stage` (use
///   `0xFFFFFFFF` as the absent sentinel — the field is
///   always present on the wire).
/// - `procedural_hit`: procedural hit group with custom
///   intersection shader plus optional closest-hit /
///   any-hit (same sentinel for absent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterRayTracingKernelGroupKind {
    #[serde(rename = "general")]
    General,

    #[serde(rename = "procedural_hit")]
    ProceduralHit,

    #[serde(rename = "triangles_hit")]
    TrianglesHit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterRayTracingKernelGroup {
    /// Stage index for `triangles_hit` / `procedural_hit`. `0xFFFFFFFF` for
    /// absent. Ignored for `general`.
    pub(crate) any_hit_stage: u32,

    /// Stage index for `triangles_hit` / `procedural_hit`. Use `0xFFFFFFFF` to
    /// indicate absent. Ignored for `general`.
    pub(crate) closest_hit_stage: u32,

    /// Stage index for `general`. `0xFFFFFFFF` for the other group kinds
    /// (ignored host-side).
    pub(crate) general_stage: u32,

    /// Stage index for `procedural_hit`. `0xFFFFFFFF` for the other group
    /// kinds. Required for `procedural_hit`.
    pub(crate) intersection_stage: u32,

    /// - `general`: contributes one ray-gen, miss, or
    ///   callable stage via `general_stage`.
    /// - `triangles_hit`: triangle hit group; sets at least
    ///   one of `closest_hit_stage` / `any_hit_stage` (use
    ///   `0xFFFFFFFF` as the absent sentinel — the field is
    ///   always present on the wire).
    /// - `procedural_hit`: procedural hit group with custom
    ///   intersection shader plus optional closest-hit /
    ///   any-hit (same sentinel for absent).
    pub(crate) kind: EscalateRequestRegisterRayTracingKernelGroupKind,
}

/// Which RT stage this SPIR-V blob fills.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterRayTracingKernelStageStage {
    #[serde(rename = "any_hit")]
    AnyHit,

    #[serde(rename = "callable")]
    Callable,

    #[serde(rename = "closest_hit")]
    ClosestHit,

    #[serde(rename = "intersection")]
    Intersection,

    #[serde(rename = "miss")]
    Miss,

    #[serde(rename = "ray_gen")]
    RayGen,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterRayTracingKernelStage {
    /// Entry-point name. Empty string is normalized to `"main"` host-side.
    pub(crate) entry_point: String,

    /// Slang source for this entry's `stage`. Mutually exclusive with
    /// `spv_hex` — exactly one of the two, absent as `""`.
    #[serde(default)]
    pub(crate) source: String,

    /// Pre-compiled SPIR-V bytecode for the stage, lowercase hex (no `0x`
    /// prefix, no whitespace). The escape hatch, mutually exclusive with
    /// `source`.
    #[serde(default)]
    pub(crate) spv_hex: String,

    /// Which RT stage this SPIR-V blob fills.
    pub(crate) stage: EscalateRequestRegisterRayTracingKernelStageStage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterRayTracingKernel {
    /// Descriptor-set-0 bindings. Validated against `rspirv-reflect` of every
    /// supplied stage at register time — mismatches return an `err` response.
    pub(crate) bindings: Vec<EscalateRequestRegisterRayTracingKernelBinding>,

    /// Shader-group layout. The order here is the order entries appear in the
    /// SBT regions (raygen / miss / hit / callable). Each variant references
    /// stage indices into `stages`.
    pub(crate) groups: Vec<EscalateRequestRegisterRayTracingKernelGroup>,

    /// Human-readable label the host gives the pipeline: it names this kernel
    /// in RHI errors and in validation-layer messages. Outside the `kernel_id`
    /// derivation — two registrations differing only in label are one pipeline,
    /// and the first one's label is what the driver keeps.
    pub(crate) label: String,

    /// Maximum ray recursion depth. Must be ≤ device's `maxRayRecursionDepth`.
    /// Most scenes (primary rays only) use 1; secondary-ray techniques bump
    /// this to 2 or more.
    pub(crate) max_recursion_depth: u32,

    /// Push-constant range size in bytes. 0 if the kernel uses no push
    /// constants. Validated against the merged shader reflection.
    pub(crate) push_constant_size: u32,

    /// Bitmask of RT stages the push-constant range is visible to. Same bit
    /// layout as `bindings.stages`. Ignored when `push_constant_size == 0`.
    pub(crate) push_constant_stages: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Shader stages composing the pipeline. Indices into this array
    /// are referenced by `groups`. At minimum a RayGen stage plus enough
    /// hit / miss stages to populate every group entry — the host's
    /// `validate_shader_groups` validates the consistency.
    pub(crate) stages: Vec<EscalateRequestRegisterRayTracingKernelStage>,
}

/// One resource bound for a single trace, named as the shaders name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunRayTracingKernelBinding {
    /// Resource kind the caller believes is at this name. Must match the
    /// kernel's reflected kind; a mismatch is an `err` response raised before
    /// anything is submitted.
    pub(crate) kind: EscalateRayTracingBindingKind,

    /// The shaders' own name for the binding. Resolved against the kernel's
    /// reflected bindings — a name the shaders do not declare, or a declared
    /// name this array omits, is an `err` response.
    pub(crate) name: String,

    /// What to bind. For `acceleration_structure` this is an `as_id` from a
    /// prior `register_acceleration_structure_tlas`; for every other kind it is
    /// a surface id the host resolves through
    /// `resolve_texture_registration_by_surface_id`.
    pub(crate) target_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunRayTracingKernel {
    /// Per-trace bindings, each named as the shaders name it. `kind` must match
    /// the kind reflection found at register time, and decides how the host
    /// resolves `target_id`:
    /// - `acceleration_structure`: an `as_id` from a prior
    ///   `register_acceleration_structure_tlas`, resolved through
    ///   `GpuContext`'s acceleration-structure registry.
    /// - `sampled_texture` / `storage_image`: a surface id the host resolves to
    ///   a device texture, the same convention compute and graphics use.
    /// - `storage_buffer` / `uniform_buffer`: refused — a trace cannot name a
    ///   surface for a buffer binding.
    ///
    /// Bindings do not persist between traces, so every trace supplies all of
    /// them.
    pub(crate) bindings: Vec<EscalateRequestRunRayTracingKernelBinding>,

    /// vkCmdTraceRaysKHR depth (usually 1 for 2D output).
    pub(crate) depth: u32,

    /// vkCmdTraceRaysKHR height.
    pub(crate) height: u32,

    /// Handle returned by a prior `register_ray_tracing_kernel` response. The
    /// host looks up the cached `Arc<VulkanRayTracingKernel>` and dispatches
    /// against it. Dispatching with an unrecognized kernel_id returns an `err`
    /// response.
    pub(crate) kernel_id: String,

    /// Push-constant payload for this dispatch, lowercase hex. Length
    /// in bytes (after hex decoding) must equal the kernel's declared
    /// `push_constant_size`. Empty string when the kernel has no push
    /// constants.
    pub(crate) push_constants_hex: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// vkCmdTraceRaysKHR width.
    pub(crate) width: u32,
}
