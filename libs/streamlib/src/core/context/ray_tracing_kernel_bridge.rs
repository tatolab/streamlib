// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side dispatch trait the escalate handler uses to drive ray-tracing
//! kernel + acceleration-structure registration and per-trace invocation
//! on behalf of subprocess customers.
//!
//! Mirrors the [`super::compute_kernel_bridge::ComputeKernelBridge`] (#550)
//! and [`super::graphics_kernel_bridge::GraphicsKernelBridge`] (#656)
//! shapes: the subprocess sends a typed IPC, the host runs privileged
//! Vulkan work via its [`crate::core::context::GpuContextFullAccess`], and
//! the bridge keeps the FullAccess capability boundary on the host side
//! of the IPC seam.
//!
//! Ray-tracing has two register ops where compute and graphics have one:
//! the bridge owns BLAS + TLAS construction (via
//! [`crate::vulkan::rhi::VulkanAccelerationStructure`]) AND kernel
//! construction (via [`crate::vulkan::rhi::VulkanRayTracingKernel`]).
//! Subprocess customers send opaque `as_id` / `kernel_id` handles plus
//! per-trace push constants, never raw `vkalia` calls.
//!
//! The trait lives here (in `streamlib`) because the escalate IPC
//! handler is here. Implementations live in application setup glue (or
//! in `streamlib-adapter-vulkan` test utilities) — those can depend on
//! `streamlib`; the reverse cannot. Register an impl via
//! [`crate::core::context::GpuContext::set_ray_tracing_kernel_bridge`]
//! before spawning subprocesses that issue
//! `register_ray_tracing_kernel` / `run_ray_tracing_kernel`.

#![cfg(target_os = "linux")]

/// Resource kind for a binding slot in the RT kernel's descriptor set 0.
/// Wire-format mirror of [`crate::core::rhi::RayTracingBindingKind`]
/// decoupled from the generated JTD types so the bridge surface is
/// stable across schema regenerations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingBindingKindWire {
    StorageBuffer,
    UniformBuffer,
    SampledTexture,
    StorageImage,
    AccelerationStructure,
}

/// One binding declaration for register-time validation against
/// SPIR-V reflection.
#[derive(Debug, Clone, Copy)]
pub struct RayTracingBindingDecl {
    pub binding: u32,
    pub kind: RayTracingBindingKindWire,
    /// Stage-visibility bitmask (`1=RAYGEN`, `2=MISS`, `4=CLOSEST_HIT`,
    /// `8=ANY_HIT`, `16=INTERSECTION`, `32=CALLABLE`).
    pub stages: u32,
}

/// Which RT stage a SPIR-V blob fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingShaderStageWire {
    RayGen,
    Miss,
    ClosestHit,
    AnyHit,
    Intersection,
    Callable,
}

/// One stage of a ray-tracing pipeline. Owned mirror of
/// [`crate::core::rhi::RayTracingStage`].
#[derive(Debug, Clone)]
pub struct RayTracingStageDecl {
    pub stage: RayTracingShaderStageWire,
    pub spv: Vec<u8>,
    /// Empty string is normalized to `"main"` host-side.
    pub entry_point: String,
}

/// Sentinel value the wire format uses to mean "this optional stage
/// index is absent" (JTD has no `Option<uint32>`). Mirrors what the
/// subprocess SDK serializes for absent group fields.
pub const RAY_TRACING_STAGE_INDEX_NONE: u32 = u32::MAX;

/// One shader group entry. Mirrors
/// [`crate::core::rhi::RayTracingShaderGroup`] but uses sentinel-encoded
/// optional fields rather than `Option<u32>` so the wire shape and the
/// bridge-domain shape line up 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingShaderGroupWire {
    /// Contributes one ray-gen, miss, or callable stage.
    General {
        /// Index into [`RayTracingKernelRegisterDecl::stages`].
        general_stage: u32,
    },
    /// Triangle hit group: closest-hit and/or any-hit shader against
    /// the built-in triangle intersection test. At least one of the
    /// two stage indices must be set (non-sentinel).
    TrianglesHit {
        closest_hit_stage: Option<u32>,
        any_hit_stage: Option<u32>,
    },
    /// Procedural hit group: a custom intersection shader plus optional
    /// closest-hit and any-hit shaders.
    ProceduralHit {
        intersection_stage: u32,
        closest_hit_stage: Option<u32>,
        any_hit_stage: Option<u32>,
    },
}

/// Full register-time descriptor passed to
/// [`RayTracingKernelBridge::register_kernel`]. Owned mirror of the
/// wire shape.
#[derive(Debug, Clone)]
pub struct RayTracingKernelRegisterDecl {
    pub label: String,
    pub stages: Vec<RayTracingStageDecl>,
    pub groups: Vec<RayTracingShaderGroupWire>,
    pub bindings: Vec<RayTracingBindingDecl>,
    pub push_constant_size: u32,
    pub push_constant_stages: u32,
    pub max_recursion_depth: u32,
}

/// One TLAS instance descriptor passed to
/// [`RayTracingKernelBridge::register_tlas`]. The `blas_id` field
/// references a previously-registered BLAS; the bridge resolves it
/// against its own `as_id → Arc<VulkanAccelerationStructure>` map.
#[derive(Debug, Clone)]
pub struct TlasInstanceDeclWire {
    pub blas_id: String,
    /// Row-major 3×4 affine transform — exactly 12 floats laid out as
    /// `[m00, m01, m02, m03, m10, ..., m23]`. Matches
    /// `VkTransformMatrixKHR` directly.
    pub transform: [[f32; 4]; 3],
    pub custom_index: u32,
    pub mask: u8,
    pub sbt_record_offset: u32,
    /// `VkGeometryInstanceFlagsKHR` bitmask passed through unchanged.
    pub flags: u32,
}

/// Full BLAS register call passed to
/// [`RayTracingKernelBridge::register_blas`].
#[derive(Debug, Clone)]
pub struct BlasRegisterDecl {
    pub label: String,
    /// Interleaved `[x, y, z, x, y, z, ...]` (R32G32B32_SFLOAT, stride
    /// 12 bytes). Length must be a multiple of 3.
    pub vertices: Vec<f32>,
    /// Three indices per triangle. Length must be a multiple of 3.
    pub indices: Vec<u32>,
}

/// Full TLAS register call passed to
/// [`RayTracingKernelBridge::register_tlas`].
#[derive(Debug, Clone)]
pub struct TlasRegisterDecl {
    pub label: String,
    pub instances: Vec<TlasInstanceDeclWire>,
}

/// Per-trace binding value passed to
/// [`RayTracingKernelBridge::run_kernel`]. `target_id` is interpreted
/// based on `kind`:
/// - [`RayTracingBindingKindWire::AccelerationStructure`]: an `as_id`
///   from a prior [`RayTracingKernelBridge::register_tlas`].
/// - all other kinds: the surface-share UUID of a host-side
///   `RhiPixelBuffer` / `StreamTexture` (same convention compute and
///   graphics use).
#[derive(Debug, Clone)]
pub struct RayTracingBindingValue {
    pub binding: u32,
    pub kind: RayTracingBindingKindWire,
    pub target_id: String,
}

/// Full per-trace input passed to [`RayTracingKernelBridge::run_kernel`].
#[derive(Debug, Clone)]
pub struct RayTracingKernelRunDispatch {
    pub kernel_id: String,
    pub bindings: Vec<RayTracingBindingValue>,
    pub push_constants: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

/// Dispatch trait the host runtime uses to drive ray-tracing
/// acceleration-structure construction, kernel registration, and
/// per-trace invocation for subprocess customers.
///
/// RT dispatch on the host is synchronous: the bridge's `run_kernel`
/// blocks on the kernel's fence inside
/// [`crate::vulkan::rhi::VulkanRayTracingKernel::trace_rays`] before
/// returning, so by the time this returns, the GPU work has retired
/// and the host's writes to the output storage image are visible to
/// any subsequent submission against the same VkDevice. The
/// subprocess can safely advance its surface-share timeline on
/// receipt of the `ok` response.
pub trait RayTracingKernelBridge: Send + Sync {
    /// Build a bottom-level acceleration structure from triangle
    /// geometry. Returns a stable `as_id` — re-registering identical
    /// geometry is allowed to hit a cache, but that is an
    /// implementation choice; AS construction is rare enough in
    /// practice that the bridge can simply build a fresh BLAS and
    /// return a fresh id.
    fn register_blas(
        &self,
        decl: &BlasRegisterDecl,
    ) -> Result<String, String>;

    /// Build a top-level acceleration structure from a list of
    /// instances referencing previously-registered BLASes. Returns a
    /// stable `as_id`. The TLAS implementation must keep the
    /// referenced BLASes alive for its lifetime (Vulkan spec).
    fn register_tlas(
        &self,
        decl: &TlasRegisterDecl,
    ) -> Result<String, String>;

    /// Register a ray-tracing kernel. Returns a stable `kernel_id` —
    /// re-registering an identical descriptor (same SPIR-V stages,
    /// same group layout, same bindings, same push-constants) hits
    /// the host-side cache and returns the same id without
    /// re-reflecting or rebuilding the pipeline.
    ///
    /// The recommended `kernel_id` shape is SHA-256 hex over a
    /// canonical byte representation of the inputs that *materially*
    /// determine the host-side `VulkanRayTracingKernel`.
    fn register_kernel(
        &self,
        decl: &RayTracingKernelRegisterDecl,
    ) -> Result<String, String>;

    /// Run one trace against a previously-registered kernel.
    ///
    /// Resolves binding `target_id`s through the application-provided
    /// resolver (UUID → `RhiPixelBuffer` / `StreamTexture` for non-AS
    /// kinds; `as_id` → `Arc<VulkanAccelerationStructure>` for the AS
    /// kind), then submits + waits on the kernel's own command
    /// buffer + fence. Errors include unrecognized `kernel_id`,
    /// target lookup failure, push-constant size mismatch, and
    /// Vulkan submit failure.
    fn run_kernel(
        &self,
        dispatch: &RayTracingKernelRunDispatch,
    ) -> Result<(), String>;
}
