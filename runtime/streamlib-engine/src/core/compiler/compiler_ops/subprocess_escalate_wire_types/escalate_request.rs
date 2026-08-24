// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Requests a helper process sends to the host over the escalate bridge.
//!
//! Wire contract: the helper builds these documents as plain Python dicts, so
//! serde's encoding of these types is the whole agreement between the two
//! sides. Field names, variant spellings and optional-field omission are not
//! free to change.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Polyglot subprocess escalate-on-behalf request (subprocess → host)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub(crate) enum EscalateRequest {
    #[serde(rename = "acquire_image")]
    AcquireImage(EscalateRequestAcquireImage),

    #[serde(rename = "acquire_pixel_buffer")]
    AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer),

    #[serde(rename = "acquire_texture")]
    AcquireTexture(EscalateRequestAcquireTexture),

    #[serde(rename = "close_processor_owned_window")]
    CloseProcessorOwnedWindow(EscalateRequestCloseProcessorOwnedWindow),

    #[serde(rename = "copy_device_export_staging_back_to_surface")]
    CopyDeviceExportStagingBackToSurface(EscalateRequestCopyDeviceExportStagingBackToSurface),

    #[serde(rename = "create_processor_owned_window")]
    CreateProcessorOwnedWindow(EscalateRequestCreateProcessorOwnedWindow),

    #[serde(rename = "drain_processor_owned_window_events")]
    DrainProcessorOwnedWindowEvents(EscalateRequestDrainProcessorOwnedWindowEvents),

    #[serde(rename = "log")]
    Log(EscalateRequestLog),

    #[serde(rename = "open_cpu_readback_staging")]
    OpenCpuReadbackStaging(EscalateRequestOpenCpuReadbackStaging),

    #[serde(rename = "open_device_export_staging")]
    OpenDeviceExportStaging(EscalateRequestOpenDeviceExportStaging),

    #[serde(rename = "refill_device_export_staging")]
    RefillDeviceExportStaging(EscalateRequestRefillDeviceExportStaging),

    #[serde(rename = "register_acceleration_structure_blas")]
    RegisterAccelerationStructureBlas(EscalateRequestRegisterAccelerationStructureBlas),

    #[serde(rename = "register_acceleration_structure_tlas")]
    RegisterAccelerationStructureTlas(EscalateRequestRegisterAccelerationStructureTlas),

    #[serde(rename = "register_compute_kernel")]
    RegisterComputeKernel(EscalateRequestRegisterComputeKernel),

    #[serde(rename = "register_graphics_kernel")]
    RegisterGraphicsKernel(EscalateRequestRegisterGraphicsKernel),

    #[serde(rename = "register_ray_tracing_kernel")]
    RegisterRayTracingKernel(EscalateRequestRegisterRayTracingKernel),

    #[serde(rename = "release_handle")]
    ReleaseHandle(EscalateRequestReleaseHandle),

    #[serde(rename = "run_compute_kernel")]
    RunComputeKernel(EscalateRequestRunComputeKernel),

    #[serde(rename = "run_compute_kernel_batch")]
    RunComputeKernelBatch(EscalateRequestRunComputeKernelBatch),

    #[serde(rename = "run_cpu_readback_copy")]
    RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy),

    #[serde(rename = "run_graphics_draw")]
    RunGraphicsDraw(EscalateRequestRunGraphicsDraw),

    #[serde(rename = "run_ray_tracing_kernel")]
    RunRayTracingKernel(EscalateRequestRunRayTracingKernel),

    #[serde(rename = "show_surface_on_processor_owned_window")]
    ShowSurfaceOnProcessorOwnedWindow(EscalateRequestShowSurfaceOnProcessorOwnedWindow),

    #[serde(rename = "try_run_cpu_readback_copy")]
    TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy),

    #[serde(rename = "wait_device_idle")]
    WaitDeviceIdle(EscalateRequestWaitDeviceIdle),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquireImage {
    /// Texture format identifier. Lowercase snake-case names: bgra8_unorm,
    /// bgra8_unorm_srgb, rgba8_unorm, rgba8_unorm_srgb. The host
    /// backs this with a render-target-capable VkImage allocated via
    /// VK_EXT_image_drm_format_modifier and a tiled DRM modifier picked
    /// from the EGL `external_only=FALSE` list — the resulting DMA-BUF can
    /// be imported by the consumer as a GL_TEXTURE_2D color attachment.
    /// Returns an error when the EGL probe didn't find an RT-capable modifier
    /// for `format` (no fallback to LINEAR — sampler-only on NVIDIA, see
    /// docs/learnings/nvidia-egl-dmabuf-render-target.md).
    /// Internal host primitive — surface adapters (streamlib-adapter-vulkan /
    /// -opengl / -skia) use this on customers' behalf; customers never invoke
    /// acquire_image directly.
    pub(crate) format: String,

    /// Pixel height of the image.
    pub(crate) height: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Pixel width of the image.
    pub(crate) width: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquirePixelBuffer {
    /// Pixel format identifier (e.g. bgra32, nv12_video_range, gray8).
    pub(crate) format: String,

    /// Pixel height of the buffer.
    pub(crate) height: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Pixel width of the buffer.
    pub(crate) width: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquireTexture {
    /// Texture format identifier. Lowercase snake-case names: rgba8_unorm,
    /// rgba8_unorm_srgb, bgra8_unorm, bgra8_unorm_srgb, rgba16_float,
    /// rgba32_float, nv12.
    pub(crate) format: String,

    /// Pixel height of the texture.
    pub(crate) height: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Usage flags the texture must support. Non-empty array of lowercase
    /// snake-case tokens drawn from: copy_src, copy_dst, texture_binding,
    /// storage_binding, render_attachment. Host validates — unknown tokens
    /// return an error response.
    pub(crate) usage: Vec<String>,

    /// Pixel width of the texture.
    pub(crate) width: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestCloseProcessorOwnedWindow {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The window to release, as `create_processor_owned_window` named it.
    /// Never an error for a window already closed, and the id stays this
    /// processor's — closed — until teardown. Answers
    /// `processor_owned_window_is_closed`.
    pub(crate) window_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestCopyDeviceExportStagingBackToSurface {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Same surface as `open_device_export_staging.surface_id`. Publishes
    /// a device-side edit: the host copies the staging buffer back into the
    /// surface's own allocation so every other holder observes it, and signals
    /// `refill_done` at end-of-submit. Refused when the surface's export is
    /// read-only — the write-back path belongs to surfaces whose only backing
    /// is their own pooled allocation. Answers with the signalled
    /// `timeline_value`.
    pub(crate) surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestCreateProcessorOwnedWindow {
    /// Requested initial height of the drawable area, in physical pixels.
    /// The window server is free to hand back another extent; the response
    /// carries what was actually minted.
    pub(crate) initial_height_in_physical_pixels: u32,

    /// Requested initial width of the drawable area, in physical pixels.
    pub(crate) initial_width_in_physical_pixels: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Window title, owned by the requesting processor. Answers with the
    /// window id every other present-class op names, plus the extent actually
    /// minted. Refused outside the helper's `setup` hook, and refused with
    /// the pump's own error when the process can get no window at all.
    pub(crate) window_title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestDrainProcessorOwnedWindowEvents {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The window whose events to drain, as `create_processor_owned_window`
    /// named it. Answers with the coalesced state: `width` / `height` the
    /// window's current extent, `close_requested_by_user` true once per
    /// gesture (this drain clears it), `processor_owned_window_is_closed`
    /// sticky.
    pub(crate) window_id: String,
}

/// Severity level of the record. Maps 1:1 onto tracing::Level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestLogLevel {
    #[serde(rename = "debug")]
    Debug,

    #[serde(rename = "error")]
    Error,

    #[serde(rename = "info")]
    Info,

    #[serde(rename = "trace")]
    Trace,

    #[serde(rename = "warn")]
    Warn,
}

/// Origin runtime of the record. Always "python" on the wire — Rust never
/// routes through escalate; Rust call sites hit `tracing::*!()` directly on
/// the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestLogSource {
    #[serde(rename = "python")]
    Python,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestLog {
    /// User-supplied structured fields. Copied flat onto the emitted
    /// RuntimeLogEvent's `attrs` map — not nested under an `attrs.key` path in
    /// the JSONL.
    pub(crate) attrs: HashMap<String, Option<Value>>,

    /// Interceptor channel when `intercepted: true`. Conventional values:
    /// "stdout", "stderr", "console.log", "logging", "fd1", "fd2". Null when
    /// `intercepted: false`.
    pub(crate) channel: Option<String>,

    /// True when the record was captured from subprocess stdout/stderr,
    /// console.log, root logging handler, or a raw fd write, rather than a
    /// direct `streamlib.log.*` call.
    pub(crate) intercepted: bool,

    /// Severity level of the record. Maps 1:1 onto tracing::Level.
    pub(crate) level: EscalateRequestLogLevel,

    /// Primary human-readable message.
    pub(crate) message: String,

    /// Pipeline identifier. Null for runtime-level records.
    pub(crate) pipeline_id: Option<String>,

    /// Processor identifier. Null outside a processor.
    pub(crate) processor_id: Option<String>,

    /// Origin runtime of the record. Always "python" on the wire — Rust never
    /// routes through escalate; Rust call sites hit `tracing::*!()` directly on
    /// the host.
    pub(crate) source: EscalateRequestLogSource,

    /// Subprocess-monotonic sequence number (u64 as string — JSON has no
    /// native u64). Escape hatch for recovering subprocess-local order within
    /// a single source. Not authoritative across sources — use `host_ts` for
    /// merged-stream ordering.
    pub(crate) source_seq: String,

    /// Subprocess wall-clock timestamp ISO8601 (advisory). Never used for
    /// ordering; the host stamps `host_ts` on receipt as the authoritative
    /// sort key.
    pub(crate) source_ts: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestOpenCpuReadbackStaging {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The surface whose pixels the subprocess wants to read with the CPU.
    /// Same contract as `open_device_export_staging` — the host allocates the
    /// staging if the surface has none, registers it plus its `refill_done`
    /// timeline with the surface-share service, and answers with the id they
    /// are registered under — differing in one respect: this staging is
    /// HOST_VISIBLE and HOST_COHERENT, so the consumer maps and reads it
    /// directly instead of importing it into a device API. A surface can carry
    /// both at once; they are separate allocations under separate ids.
    /// The `ok` response carries `handle_id`, `width`, `height`, `format`,
    /// `staging_byte_size`, `bytes_per_row`, `writable`, and
    /// `exporting_device_uuid`. The staging fd and the timeline fd travel over
    /// the surface-share socket at check-out, never over this one.
    pub(crate) surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestOpenDeviceExportStaging {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The surface whose pixels the subprocess wants in an external device
    /// API's dialect (CUDA today). The host allocates that surface's OPAQUE_FD
    /// device-export staging buffer if it has none, registers the staging plus
    /// its `refill_done` timeline with the surface-share service, and answers
    /// with the id they are registered under. Pool allocations are DMA-BUF-
    /// flavoured and external device APIs import OPAQUE_FD; one allocation
    /// cannot export both on NVIDIA, which is why the staging exists at all.
    /// The `ok` response carries `handle_id` (the surface-share id to check
    /// out), `width`, `height`, `format`, `staging_byte_size`, `bytes_per_row`,
    /// `writable`, and `exporting_device_uuid`. The staging fd and the timeline
    /// fd travel over the surface-share socket at check-out, never over this
    /// one.
    pub(crate) surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRefillDeviceExportStaging {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Same surface as `open_device_export_staging.surface_id`. The
    /// host resolves the blit source fresh, copies the surface's current
    /// pixels into the staging buffer, and signals a new value on the
    /// staging's `refill_done` timeline at end-of-submit. The response's
    /// `timeline_value` is what the subprocess waits for on its imported
    /// `ConsumerVulkanTimelineSemaphore` before reading the staging. No FDs
    /// travel on the wire.
    /// The source is resolved per refill and never cached: rotating producers
    /// re-register a different texture under the same surface id every frame,
    /// so a cached source blits the previous cycle's pixels.
    pub(crate) surface_id: String,
}

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

/// Resource kind for a compute binding slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EscalateComputeBindingKind {
    #[serde(rename = "sampled_image")]
    SampledImage,

    #[serde(rename = "sampled_texture")]
    SampledTexture,

    #[serde(rename = "storage_buffer")]
    StorageBuffer,

    #[serde(rename = "storage_image")]
    StorageImage,

    #[serde(rename = "uniform_buffer")]
    UniformBuffer,
}

impl EscalateComputeBindingKind {
    /// This kind's spelling on the wire — what a register response hands back
    /// and the next dispatch echoes.
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::SampledImage => "sampled_image",
            Self::SampledTexture => "sampled_texture",
            Self::StorageBuffer => "storage_buffer",
            Self::StorageImage => "storage_image",
            Self::UniformBuffer => "uniform_buffer",
        }
    }
}

/// One binding a compute kernel declares at registration, named as the shader
/// names it. The slot number is not on the wire — it comes from reflection,
/// and the name is what a dispatch resolves against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterComputeKernelBinding {
    /// Resource kind the caller expects at this name. Checked against
    /// reflection at registration; a mismatch is an `err` response.
    pub(crate) kind: EscalateComputeBindingKind,

    /// The shader's own name for the binding.
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterComputeKernel {
    /// Binding declarations, by name. Validated against `rspirv-reflect` of
    /// the SPIR-V: a name the shader does not declare, a declared name the
    /// array omits, or a kind that disagrees with reflection each return an
    /// `err` response. Empty array means the caller declares nothing and the
    /// reflected shape stands on its own.
    pub(crate) bindings: Vec<EscalateRequestRegisterComputeKernelBinding>,

    /// Push-constant range size in bytes. 0 if the shader uses no push
    /// constants. The host validates this against the shader's reflected push-
    /// constant range and rejects mismatches with an `err` response.
    pub(crate) push_constant_size: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// GLSL source for the compute shader; the engine compiles it. Mutually
    /// exclusive with `spv_hex` — exactly one of the two, absent as `""`.
    #[serde(default)]
    pub(crate) source: String,

    /// Which stage `source` compiles for. Empty is normalized to `compute`,
    /// and anything else is refused: this op registers a compute kernel, and a
    /// stage that disagrees with the op is a caller mistake, not a variant.
    /// Carried because the stage is part of the compilation cache key.
    #[serde(default)]
    pub(crate) stage: String,

    /// Entry-point name. Empty string is normalized to `"main"` host-side. A
    /// GLSL `source` supports no other value — glslang will not rename an
    /// entry point — so a non-`main` name is meaningful only with `spv_hex`.
    #[serde(default)]
    pub(crate) entry_point: String,

    /// Pre-compiled SPIR-V bytecode for the compute shader, encoded as
    /// lowercase hex (no `0x` prefix, no whitespace). The escape hatch for a
    /// caller that already has a module; mutually exclusive with `source`.
    /// The host parses the bytes back,
    /// derives the binding shape from `rspirv-reflect`, and constructs a
    /// `VulkanComputeKernel` via `GpuContext::create_compute_kernel`.
    /// Re-registering an identical kernel is a host-side cache hit — no
    /// re-reflection, no fresh pipeline, and the same `kernel_id` back.
    /// The host's `VulkanComputeKernel` also persists driver- compiled pipeline
    /// state to `<XDG_CACHE_HOME>/streamlib/ pipeline-cache/<spirv_hash>.bin`,
    /// so first-inference latency after a host process restart is fast on user-
    /// registered ML kernels.
    ///
    /// The blob must retain its `OpName` decorations (`glslc -g`): bindings
    /// resolve by name, so a stripped blob is refused at registration. What
    /// the engine compiles from `source` keeps them by construction — it emits
    /// debug info, which carries the names through the optimizer.
    #[serde(default)]
    pub(crate) spv_hex: String,
}

/// Resource kind for a graphics binding slot.
///
/// One enum for the register array, the draw array and the register response —
/// they name the same four kinds, and two spellings of one set is two things to
/// keep in lockstep forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EscalateGraphicsBindingKind {
    #[serde(rename = "sampled_texture")]
    SampledTexture,

    #[serde(rename = "storage_buffer")]
    StorageBuffer,

    #[serde(rename = "storage_image")]
    StorageImage,

    #[serde(rename = "uniform_buffer")]
    UniformBuffer,
}

impl EscalateGraphicsBindingKind {
    /// This kind's spelling on the wire — what a register response hands back
    /// and the next draw echoes.
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::SampledTexture => "sampled_texture",
            Self::StorageBuffer => "storage_buffer",
            Self::StorageImage => "storage_image",
            Self::UniformBuffer => "uniform_buffer",
        }
    }
}

/// One binding a graphics kernel declares at registration, named as the shaders
/// name it. The slot number is not on the wire — it comes from reflection, and
/// the name is what a draw resolves against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterGraphicsKernelBinding {
    /// Resource kind the caller expects at this name. Checked against
    /// reflection at registration; a mismatch is an `err` response.
    pub(crate) kind: EscalateGraphicsBindingKind,

    /// The shaders' own name for the binding.
    pub(crate) name: String,

    /// Bitmask of stages the binding is visible to. `1 = VERTEX`, `2 =
    /// FRAGMENT`, `3 = VERTEX_FRAGMENT`. `0` asserts nothing about stages.
    ///
    /// A declaration may widen a binding's visibility past what the shaders
    /// read, never narrow it below; naming a stage this kernel has no module
    /// for is refused at registration, where the multi-stage declaration is
    /// built.
    pub(crate) stages: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp {
    #[serde(rename = "add")]
    Add,

    #[serde(rename = "max")]
    Max,

    #[serde(rename = "min")]
    Min,

    #[serde(rename = "reverse_subtract")]
    ReverseSubtract,

    #[serde(rename = "subtract")]
    Subtract,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp {
    #[serde(rename = "add")]
    Add,

    #[serde(rename = "max")]
    Max,

    #[serde(rename = "min")]
    Min,

    #[serde(rename = "reverse_subtract")]
    ReverseSubtract,

    #[serde(rename = "subtract")]
    Subtract,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor {
    #[serde(rename = "constant_alpha")]
    ConstantAlpha,

    #[serde(rename = "constant_color")]
    ConstantColor,

    #[serde(rename = "dst_alpha")]
    DstAlpha,

    #[serde(rename = "dst_color")]
    DstColor,

    #[serde(rename = "one")]
    One,

    #[serde(rename = "one_minus_constant_alpha")]
    OneMinusConstantAlpha,

    #[serde(rename = "one_minus_constant_color")]
    OneMinusConstantColor,

    #[serde(rename = "one_minus_dst_alpha")]
    OneMinusDstAlpha,

    #[serde(rename = "one_minus_dst_color")]
    OneMinusDstColor,

    #[serde(rename = "one_minus_src_alpha")]
    OneMinusSrcAlpha,

    #[serde(rename = "one_minus_src_color")]
    OneMinusSrcColor,

    #[serde(rename = "src_alpha")]
    SrcAlpha,

    #[serde(rename = "src_alpha_saturate")]
    SrcAlphaSaturate,

    #[serde(rename = "src_color")]
    SrcColor,

    #[serde(rename = "zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor {
    #[serde(rename = "constant_alpha")]
    ConstantAlpha,

    #[serde(rename = "constant_color")]
    ConstantColor,

    #[serde(rename = "dst_alpha")]
    DstAlpha,

    #[serde(rename = "dst_color")]
    DstColor,

    #[serde(rename = "one")]
    One,

    #[serde(rename = "one_minus_constant_alpha")]
    OneMinusConstantAlpha,

    #[serde(rename = "one_minus_constant_color")]
    OneMinusConstantColor,

    #[serde(rename = "one_minus_dst_alpha")]
    OneMinusDstAlpha,

    #[serde(rename = "one_minus_dst_color")]
    OneMinusDstColor,

    #[serde(rename = "one_minus_src_alpha")]
    OneMinusSrcAlpha,

    #[serde(rename = "one_minus_src_color")]
    OneMinusSrcColor,

    #[serde(rename = "src_alpha")]
    SrcAlpha,

    #[serde(rename = "src_alpha_saturate")]
    SrcAlphaSaturate,

    #[serde(rename = "src_color")]
    SrcColor,

    #[serde(rename = "zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor {
    #[serde(rename = "constant_alpha")]
    ConstantAlpha,

    #[serde(rename = "constant_color")]
    ConstantColor,

    #[serde(rename = "dst_alpha")]
    DstAlpha,

    #[serde(rename = "dst_color")]
    DstColor,

    #[serde(rename = "one")]
    One,

    #[serde(rename = "one_minus_constant_alpha")]
    OneMinusConstantAlpha,

    #[serde(rename = "one_minus_constant_color")]
    OneMinusConstantColor,

    #[serde(rename = "one_minus_dst_alpha")]
    OneMinusDstAlpha,

    #[serde(rename = "one_minus_dst_color")]
    OneMinusDstColor,

    #[serde(rename = "one_minus_src_alpha")]
    OneMinusSrcAlpha,

    #[serde(rename = "one_minus_src_color")]
    OneMinusSrcColor,

    #[serde(rename = "src_alpha")]
    SrcAlpha,

    #[serde(rename = "src_alpha_saturate")]
    SrcAlphaSaturate,

    #[serde(rename = "src_color")]
    SrcColor,

    #[serde(rename = "zero")]
    Zero,
}

/// Blend factor. Ignored when `color_blend_enabled` is false; carry a valid
/// value (e.g. `one`) regardless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor {
    #[serde(rename = "constant_alpha")]
    ConstantAlpha,

    #[serde(rename = "constant_color")]
    ConstantColor,

    #[serde(rename = "dst_alpha")]
    DstAlpha,

    #[serde(rename = "dst_color")]
    DstColor,

    #[serde(rename = "one")]
    One,

    #[serde(rename = "one_minus_constant_alpha")]
    OneMinusConstantAlpha,

    #[serde(rename = "one_minus_constant_color")]
    OneMinusConstantColor,

    #[serde(rename = "one_minus_dst_alpha")]
    OneMinusDstAlpha,

    #[serde(rename = "one_minus_dst_color")]
    OneMinusDstColor,

    #[serde(rename = "one_minus_src_alpha")]
    OneMinusSrcAlpha,

    #[serde(rename = "one_minus_src_color")]
    OneMinusSrcColor,

    #[serde(rename = "src_alpha")]
    SrcAlpha,

    #[serde(rename = "src_alpha_saturate")]
    SrcAlphaSaturate,

    #[serde(rename = "src_color")]
    SrcColor,

    #[serde(rename = "zero")]
    Zero,
}

/// Depth compare op. Ignored when `depth_stencil_enabled` is false; the
/// wire field must still carry a valid value (use `always` as the default
/// placeholder when disabled).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp {
    #[serde(rename = "always")]
    Always,

    #[serde(rename = "equal")]
    Equal,

    #[serde(rename = "greater")]
    Greater,

    #[serde(rename = "greater_or_equal")]
    GreaterOrEqual,

    #[serde(rename = "less")]
    Less,

    #[serde(rename = "less_or_equal")]
    LessOrEqual,

    #[serde(rename = "never")]
    Never,

    #[serde(rename = "not_equal")]
    NotEqual,
}

/// Which pipeline state is set dynamically per draw vs baked into the pipeline
/// at creation. `none` bakes a default 1×1 viewport (offscreen fixed-size
/// only); `viewport_scissor` lets the same pipeline serve varying extents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState {
    #[serde(rename = "none")]
    None,

    #[serde(rename = "viewport_scissor")]
    ViewportScissor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode {
    #[serde(rename = "back")]
    Back,

    #[serde(rename = "front")]
    Front,

    #[serde(rename = "front_and_back")]
    FrontAndBack,

    #[serde(rename = "none")]
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace {
    #[serde(rename = "clockwise")]
    Clockwise,

    #[serde(rename = "counter_clockwise")]
    CounterClockwise,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode {
    #[serde(rename = "fill")]
    Fill,

    #[serde(rename = "line")]
    Line,

    #[serde(rename = "point")]
    Point,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateTopology {
    #[serde(rename = "line_list")]
    LineList,

    #[serde(rename = "line_strip")]
    LineStrip,

    #[serde(rename = "point_list")]
    PointList,

    #[serde(rename = "triangle_fan")]
    TriangleFan,

    #[serde(rename = "triangle_list")]
    TriangleList,

    #[serde(rename = "triangle_strip")]
    TriangleStrip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat {
    #[serde(rename = "r32_float")]
    R32Float,

    #[serde(rename = "r32_sint")]
    R32Sint,

    #[serde(rename = "r32_uint")]
    R32Uint,

    #[serde(rename = "rg32_float")]
    Rg32Float,

    #[serde(rename = "rg32_sint")]
    Rg32Sint,

    #[serde(rename = "rg32_uint")]
    Rg32Uint,

    #[serde(rename = "rgb32_float")]
    Rgb32Float,

    #[serde(rename = "rgb32_sint")]
    Rgb32Sint,

    #[serde(rename = "rgb32_uint")]
    Rgb32Uint,

    #[serde(rename = "rgba32_float")]
    Rgba32Float,

    #[serde(rename = "rgba32_sint")]
    Rgba32Sint,

    #[serde(rename = "rgba32_uint")]
    Rgba32Uint,

    #[serde(rename = "rgba8_snorm")]
    Rgba8Snorm,

    #[serde(rename = "rgba8_unorm")]
    Rgba8Unorm,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute {
    pub(crate) binding: u32,

    pub(crate) format: EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat,

    pub(crate) location: u32,

    pub(crate) offset: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate {
    #[serde(rename = "instance")]
    Instance,

    #[serde(rename = "vertex")]
    Vertex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding {
    pub(crate) binding: u32,

    pub(crate) input_rate:
        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate,

    pub(crate) stride: u32,
}

/// Depth attachment format. Absent disables depth attachments — the
/// depth_stencil flags must be consistent (`depth_stencil_enabled = false` when
/// this is absent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat {
    #[serde(rename = "d16_unorm")]
    D16Unorm,

    #[serde(rename = "d24_unorm_s8_uint")]
    D24UnormS8Uint,

    #[serde(rename = "d32_sfloat")]
    D32Sfloat,
}

/// Fixed-function pipeline state plus attachment formats for the graphics
/// pipeline. Mirrors the host `GraphicsPipelineState` shape; unsupported
/// combinations — MSAA samples > 1, other than one colour attachment, either
/// half of a depth attachment — are rejected with an `err` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterGraphicsKernelPipelineState {
    /// Color attachment texture formats (lowercase snake-case names matching
    /// `acquire_texture.format`). v1 supports a single color attachment; arrays
    /// of length other than 1 are rejected.
    pub(crate) attachment_color_formats: Vec<String>,

    pub(crate) color_blend_alpha_op:
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,

    pub(crate) color_blend_color_op:
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,

    pub(crate) color_blend_dst_alpha_factor:
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,

    pub(crate) color_blend_dst_color_factor:
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,

    pub(crate) color_blend_enabled: bool,

    pub(crate) color_blend_src_alpha_factor:
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,

    /// Blend factor. Ignored when `color_blend_enabled` is false; carry a valid
    /// value (e.g. `one`) regardless.
    pub(crate) color_blend_src_color_factor:
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,

    /// Color write mask bits — `1=R`, `2=G`, `4=B`, `8=A`. `15` (`0b1111`)
    /// writes RGBA. Used both when blending is disabled and as the blend
    /// attachment's `color_write_mask` when enabled.
    pub(crate) color_write_mask: u32,

    /// Never read: `depth_stencil_enabled` must be false, so there is no depth
    /// test to configure. Required on the wire, so send `always`.
    pub(crate) depth_compare_op: EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp,

    /// Must be false. The offscreen pass a draw runs through attaches colour
    /// targets only, so a depth-testing pipeline has no attachment to test
    /// against; true is an `err` response.
    pub(crate) depth_stencil_enabled: bool,

    /// Never read, for the same reason `depth_compare_op` is not. Send false.
    pub(crate) depth_write: bool,

    /// Which pipeline state is set dynamically per draw vs baked into the
    /// pipeline at creation. `none` bakes a default 1×1 viewport (offscreen
    /// fixed-size only); `viewport_scissor` lets the same pipeline serve
    /// varying extents.
    pub(crate) dynamic_state: EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState,

    /// MSAA sample count. Only `1` is supported in v1; any other value returns
    /// an `err` response.
    pub(crate) multisample_samples: u32,

    pub(crate) rasterization_cull_mode:
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode,

    pub(crate) rasterization_front_face:
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace,

    pub(crate) rasterization_line_width: f32,

    pub(crate) rasterization_polygon_mode:
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode,

    pub(crate) topology: EscalateRequestRegisterGraphicsKernelPipelineStateTopology,

    /// Must be empty, for the same reason `vertex_input_bindings` must be: an
    /// attribute is pulled from a binding. A non-empty array is an `err`
    /// response.
    pub(crate) vertex_input_attributes:
        Vec<EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute>,

    /// Must be empty. No escalate op mints a `VertexBuffer` to fill a binding —
    /// a helper can acquire a pixel buffer, a texture or an image, and the
    /// vertex-buffer setter takes none of them — so a pipeline pulling from one
    /// would register and then be refused at every draw. Vertices are
    /// fabricated from `gl_VertexIndex`; a non-empty array is an `err` response.
    pub(crate) vertex_input_bindings:
        Vec<EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding>,

    /// Must be absent, for the same reason `depth_stencil_enabled` must be
    /// false: the pass attaches colour targets only, so a pipeline declaring a
    /// depth format would disagree with it at every draw. A present one is an
    /// `err` response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attachment_depth_format:
        Option<EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRegisterGraphicsKernel {
    /// Descriptor-set-0 bindings the host pipeline declares. Validated against
    /// `rspirv-reflect` of the supplied SPIR-V at register time — mismatches
    /// return an `err` response. Empty array means no bindings.
    pub(crate) bindings: Vec<EscalateRequestRegisterGraphicsKernelBinding>,

    /// Depth of the descriptor-set ring. Render-loop callers pass `frame_index
    /// ∈ [0, descriptor_sets_in_flight)` per draw. Must be ≥ 1.
    pub(crate) descriptor_sets_in_flight: u32,

    /// Entry-point name for the fragment stage. Empty string is normalized to
    /// `"main"` host-side.
    pub(crate) fragment_entry_point: String,

    /// GLSL source for the fragment stage. Mutually exclusive with
    /// `fragment_spv_hex` — exactly one of the two, absent as `""`.
    #[serde(default)]
    pub(crate) fragment_source: String,

    /// Pre-compiled SPIR-V bytecode for the fragment stage, encoded as
    /// lowercase hex. The escape hatch, mutually exclusive with
    /// `fragment_source`. Today exactly one fragment stage is required
    /// (matching the host kernel's v1 contract).
    #[serde(default)]
    pub(crate) fragment_spv_hex: String,

    /// Human-readable label the host gives the pipeline: it names this kernel
    /// in RHI errors and in validation-layer messages. Outside the `kernel_id`
    /// derivation — two registrations differing only in label are one pipeline,
    /// and the first one's label is what the driver keeps.
    pub(crate) label: String,

    /// Fixed-function pipeline state plus attachment formats for the graphics
    /// pipeline. Mirrors the host `GraphicsPipelineState` shape; a shape a draw
    /// cannot run is an `err` response — MSAA, other than exactly one colour
    /// attachment, either half of a depth attachment, either half of a vertex
    /// input, or a write mask no channel owns.
    pub(crate) pipeline_state: EscalateRequestRegisterGraphicsKernelPipelineState,

    /// Push-constant range size in bytes, validated against the merged shader
    /// reflection. Set 0 if the shaders use no push constants.
    pub(crate) push_constant_size: u32,

    /// Bitmask of stages the push-constant range is visible to. `1 = VERTEX`,
    /// `2 = FRAGMENT`. Ignored when `push_constant_size == 0`.
    pub(crate) push_constant_stages: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Entry-point name for the vertex stage. Empty string is normalized to
    /// `"main"` host-side.
    pub(crate) vertex_entry_point: String,

    /// GLSL source for the vertex stage. Mutually exclusive with
    /// `vertex_spv_hex` — exactly one of the two, absent as `""`.
    #[serde(default)]
    pub(crate) vertex_source: String,

    /// Pre-compiled SPIR-V bytecode for the vertex stage, encoded as lowercase
    /// hex (no `0x` prefix, no whitespace). The escape hatch, mutually
    /// exclusive with `vertex_source`. Today exactly one vertex stage
    /// is required (the host kernel rejects zero or multiple vertex stages).
    /// Geometry / tessellation / mesh / task stages are not yet supported.
    #[serde(default)]
    pub(crate) vertex_spv_hex: String,
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

    /// GLSL source for this entry's `stage`. Mutually exclusive with
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestReleaseHandle {
    /// Opaque handle ID previously returned by acquire_*.
    pub(crate) handle_id: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}

/// One resource bound for a single dispatch, named as the shader names it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunComputeKernelBinding {
    /// Resource kind the caller believes is at this name. Must match the
    /// kernel's reflected kind; a mismatch is an `err` response raised before
    /// anything is submitted.
    pub(crate) kind: EscalateComputeBindingKind,

    /// The shader's own name for the binding. Resolved against the kernel's
    /// reflected bindings — a name the shader does not declare, or a declared
    /// name this array omits, is an `err` response.
    pub(crate) name: String,

    /// Surface id of the resource to bind, as the host registered it
    /// (`GpuContext::register_texture` / the surface-share service). The host
    /// resolves it through `resolve_texture_registration_by_surface_id`.
    pub(crate) target_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunComputeKernel {
    /// Every binding the kernel declares, supplied by name for this dispatch.
    ///
    /// Bindings are passed at dispatch and never persist on the kernel, so
    /// this array is complete every time: there is no carried-over value from
    /// the previous dispatch and no implicit default. One name appearing twice
    /// is an error, checked host-side so the caller's language is not the only
    /// guard.
    pub(crate) bindings: Vec<EscalateRequestRunComputeKernelBinding>,

    /// vkCmdDispatch groupCountX.
    pub(crate) group_count_x: u32,

    /// vkCmdDispatch groupCountY.
    pub(crate) group_count_y: u32,

    /// vkCmdDispatch groupCountZ.
    pub(crate) group_count_z: u32,

    /// Handle returned by a prior `register_compute_kernel` response. The
    /// host looks up the cached `Arc<VulkanComputeKernel>` and dispatches
    /// against it. Dispatching with an unrecognized kernel_id returns an `err`
    /// response.
    pub(crate) kernel_id: String,

    /// Push-constant payload for this dispatch, encoded as lowercase hex.
    /// Length in bytes (after hex decoding) must equal the kernel's declared
    /// `push_constant_size`. Empty string when the kernel has no push
    /// constants.
    pub(crate) push_constants_hex: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}

/// One dispatch of a `run_compute_kernel_batch`, in the order it runs.
///
/// Carries everything `run_compute_kernel` does except the request id, which
/// belongs to the batch: the whole array is one request, answered once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunComputeKernelBatchDispatch {
    /// Every binding this dispatch's kernel declares, supplied by name.
    pub(crate) bindings: Vec<EscalateRequestRunComputeKernelBinding>,

    /// vkCmdDispatch groupCountX.
    pub(crate) group_count_x: u32,

    /// vkCmdDispatch groupCountY.
    pub(crate) group_count_y: u32,

    /// vkCmdDispatch groupCountZ.
    pub(crate) group_count_z: u32,

    /// Handle returned by a prior `register_compute_kernel` response. No
    /// kernel_id may appear twice in one batch: a kernel owns a single
    /// descriptor set, so the second bind would retarget the dispatch already
    /// recorded against it. Refused with an `err` response.
    pub(crate) kernel_id: String,

    /// Push-constant payload for this dispatch alone, encoded as lowercase hex.
    pub(crate) push_constants_hex: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunComputeKernelBatch {
    /// The dispatches to record into one command buffer, in order. Each is
    /// barriered against the one before it, so a later pass observes an
    /// earlier pass's writes.
    ///
    /// The whole array is one submission and one fence wait — which is the
    /// reason the op exists, and why a multi-pass filter sends this rather
    /// than N `run_compute_kernel` requests. An empty array is accepted and
    /// submits nothing.
    pub(crate) dispatches: Vec<EscalateRequestRunComputeKernelBatchDispatch>,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}

/// Which copy the host runs. `image_to_buffer` reads the named frame into the
/// staging; `buffer_to_image` publishes the staged edit back into the surface's
/// own pooled allocation, and is refused unless that same frame was read in
/// first. Which Vulkan copy each becomes is the engine's business and varies
/// with the surface's backing.
///
/// The host signals a new value on the staging's timeline at end-of-submit; the
/// subprocess waits on the timeline (through its imported
/// `ConsumerVulkanTimelineSemaphore`) before reading or releasing. No FDs travel
/// on the wire — only the timeline value the host signaled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRunCpuReadbackCopyDirection {
    #[serde(rename = "buffer_to_image")]
    BufferToImage,

    #[serde(rename = "image_to_buffer")]
    ImageToBuffer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunCpuReadbackCopy {
    /// Which copy the host runs. `image_to_buffer` reads the named frame
    /// into the staging; `buffer_to_image` publishes the staged edit back
    /// into the surface's own pooled allocation, and is refused unless
    /// that same frame was read in first. Which Vulkan copy each becomes
    /// is the engine's business and varies with the surface's backing.
    ///
    /// The host signals a new value on the staging's timeline at
    /// end-of-submit; the subprocess waits on the timeline (through its
    /// imported `ConsumerVulkanTimelineSemaphore`) before reading or
    /// releasing. No FDs travel on the wire — only the timeline value the
    /// host signaled.
    pub(crate) direction: EscalateRequestRunCpuReadbackCopyDirection,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Any surface id the engine can resolve — a published frame
    /// (`<slot>#<generation>`) or a registered texture's id. It is not
    /// parsed as an integer and carries no registration precondition: the
    /// host mints the CPU-readable staging on first ask.
    ///
    /// `direction: image_to_buffer` reads the frame into the staging;
    /// `buffer_to_image` publishes an edit of it back, and is refused
    /// unless a read happened first — an unfilled staging holds
    /// uninitialised memory, not an edit.
    ///
    /// The subprocess reaches the staging's memory through
    /// `open_cpu_readback_staging` plus the surface-share check-out that
    /// follows it; no fd travels on this socket.
    pub(crate) surface_id: String,
}

/// One resource bound for a single draw, named as the shaders name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunGraphicsDrawBinding {
    /// Resource kind the caller believes is at this name. Must match the
    /// kernel's reflected kind; a mismatch is an `err` response raised before
    /// anything is submitted.
    pub(crate) kind: EscalateGraphicsBindingKind,

    /// The shaders' own name for the binding. Resolved against the kernel's
    /// reflected bindings — a name the shaders do not declare, or a declared
    /// name this array omits, is an `err` response.
    pub(crate) name: String,

    /// Surface id of the resource to bind, as the host registered it. The host
    /// resolves it through `resolve_texture_registration_by_surface_id`.
    pub(crate) surface_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRunGraphicsDrawDrawKind {
    #[serde(rename = "draw")]
    Draw,

    #[serde(rename = "draw_indexed")]
    DrawIndexed,
}

/// Draw call. `kind = "draw"` selects non-indexed (`vertex_count`-driven),
/// `kind = "draw_indexed"` requires `index_buffer` to be set and uses
/// `index_count` / `first_index` / `vertex_offset`. Fields not used by the
/// selected kind are ignored host-side; subprocesses should still send valid
/// placeholder values (zero is fine) to keep the wire shape regular.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunGraphicsDrawDraw {
    pub(crate) first_index: u32,

    pub(crate) first_instance: u32,

    pub(crate) first_vertex: u32,

    pub(crate) index_count: u32,

    pub(crate) instance_count: u32,

    pub(crate) kind: EscalateRequestRunGraphicsDrawDrawKind,

    pub(crate) vertex_count: u32,

    pub(crate) vertex_offset: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunGraphicsDrawVertexBuffer {
    pub(crate) binding: u32,

    pub(crate) offset: String,

    pub(crate) surface_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRunGraphicsDrawIndexBufferIndexType {
    #[serde(rename = "uint16")]
    Uint16,

    #[serde(rename = "uint32")]
    Uint32,
}

/// Required when `draw.kind == "draw_indexed"`, must be absent otherwise.
/// `surface_uuid` resolves to a `PixelBuffer`; `offset` is the byte offset
/// into it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunGraphicsDrawIndexBuffer {
    pub(crate) index_type: EscalateRequestRunGraphicsDrawIndexBufferIndexType,

    pub(crate) offset: String,

    pub(crate) surface_uuid: String,
}

/// Dynamic scissor rect for this draw. Required when the kernel declared
/// `dynamic_state = "viewport_scissor"`; ignored otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunGraphicsDrawScissor {
    pub(crate) height: u32,

    pub(crate) width: u32,

    pub(crate) x: i32,

    pub(crate) y: i32,
}

/// Dynamic viewport for this draw. Required when the kernel's pipeline state
/// declared `dynamic_state = "viewport_scissor"`; ignored otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunGraphicsDrawViewport {
    pub(crate) height: f32,

    pub(crate) max_depth: f32,

    pub(crate) min_depth: f32,

    pub(crate) width: f32,

    pub(crate) x: f32,

    pub(crate) y: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunGraphicsDraw {
    /// Per-draw bindings, each named as the shaders name it. `kind` must match
    /// the kind reflection found at register time, and `surface_uuid` must be a
    /// surface the host can resolve to a device texture. Bindings do not
    /// persist between draws, so every draw supplies all of them.
    pub(crate) bindings: Vec<EscalateRequestRunGraphicsDrawBinding>,

    /// UUIDs of color attachment textures. v1 requires exactly one entry —
    /// multi-attachment is a future extension. Each UUID must resolve to a
    /// host-side `Texture` registered as a render target.
    pub(crate) color_target_uuids: Vec<String>,

    /// Draw call. `kind = "draw"` is the only kind the host runs: it is
    /// non-indexed and `vertex_count`-driven. `kind = "draw_indexed"` is
    /// refused, because it needs an index buffer and no escalate op mints one.
    /// The indexed fields — `index_count` / `first_index` / `vertex_offset` —
    /// stay on the wire so its shape is regular; send zeros.
    pub(crate) draw: EscalateRequestRunGraphicsDrawDraw,

    /// Render-area height in pixels.
    pub(crate) extent_height: u32,

    /// Render-area width in pixels.
    pub(crate) extent_width: u32,

    /// Slot in the kernel's descriptor-set ring. Must satisfy `frame_index
    /// < descriptor_sets_in_flight` declared at register time. Render-loop
    /// callers cycle this through `MAX_FRAMES_IN_FLIGHT` so concurrent frames
    /// don't scribble each other's bindings.
    pub(crate) frame_index: u32,

    /// Handle returned by a prior `register_graphics_kernel` response. The
    /// host looks up the cached `Arc<VulkanGraphicsKernel>` and dispatches
    /// against it. Dispatching with an unrecognized kernel_id returns an `err`
    /// response.
    pub(crate) kernel_id: String,

    /// Push-constant payload for this draw, lowercase hex. Must decode to
    /// exactly the kernel's declared `push_constant_size` (or empty if zero).
    pub(crate) push_constants_hex: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Per-draw vertex buffer bindings. Always empty: the host's vertex-buffer
    /// setter takes a `VertexBuffer`, and no escalate op mints one — a helper
    /// can acquire a pixel buffer, a texture or an image, none of which that
    /// setter accepts. A non-empty array is an `err` response; a vertex stage
    /// fabricates its positions from `gl_VertexIndex` instead.
    pub(crate) vertex_buffers: Vec<EscalateRequestRunGraphicsDrawVertexBuffer>,

    /// UUID of a depth attachment texture. Always absent: the offscreen pass
    /// this op drives attaches colour targets only, so a depth attachment would
    /// never be tested against. A present one is an `err` response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) depth_target_uuid: Option<String>,

    /// Always absent, for the same reason `vertex_buffers` is always empty: the
    /// host's index-buffer setter takes an `IndexBuffer` and no escalate op
    /// mints one. A present one is an `err` response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) index_buffer: Option<EscalateRequestRunGraphicsDrawIndexBuffer>,

    /// Dynamic scissor rect for this draw. Required when the kernel declared
    /// `dynamic_state = "viewport_scissor"`; ignored otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scissor: Option<EscalateRequestRunGraphicsDrawScissor>,

    /// Dynamic viewport for this draw. Required when the kernel's pipeline
    /// state declared `dynamic_state = "viewport_scissor"`; ignored otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) viewport: Option<EscalateRequestRunGraphicsDrawViewport>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestShowSurfaceOnProcessorOwnedWindow {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The named frame's height in pixels. Zero says the caller is naming a
    /// bare surface id it knows nothing else about, which the host reads as
    /// "a buffer-backed surface is not acceptable to me" — the same refusal
    /// every other zero-extent resolution takes.
    pub(crate) source_height_in_pixels: u32,

    /// The named frame's width in pixels. Same zero-extent reading as
    /// `source_height_in_pixels`.
    pub(crate) source_width_in_pixels: u32,

    /// The published surface id naming the frame to show next. Handed to the
    /// window's present loop without waiting on it — latest-wins, so naming
    /// none leaves the last frame up. A retired id is refused as a
    /// recycled-frame error; a closed window answers
    /// `processor_owned_window_is_closed` rather than any error.
    pub(crate) surface_id: String,

    /// The window to show it on, as `create_processor_owned_window` named it.
    pub(crate) window_id: String,

    /// The producer's published `VkImageLayout` for this frame as the raw
    /// int32 enumerant, when it overrides the per-surface default. Absent
    /// when the caller names a bare surface id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) producer_published_texture_layout: Option<i32>,
}

/// Same shape and same refusals as `run_cpu_readback_copy.direction`; only the
/// response to a busy staging differs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestTryRunCpuReadbackCopyDirection {
    #[serde(rename = "buffer_to_image")]
    BufferToImage,

    #[serde(rename = "image_to_buffer")]
    ImageToBuffer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestTryRunCpuReadbackCopy {
    /// Same shape and same refusals as `run_cpu_readback_copy.direction`;
    /// only the response to a busy staging differs.
    pub(crate) direction: EscalateRequestTryRunCpuReadbackCopyDirection,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Same shape as `run_cpu_readback_copy.surface_id`. The host returns a
    /// [`super::escalate_response::EscalateResponse::Contended`] response (no
    /// timeline value, no copy executed) when another copy is already in
    /// flight against this surface's staging. Subprocess customers use this
    /// to skip a frame instead of stalling their thread runner. Every other
    /// refusal — a retired frame id, a read-only export, an unfilled
    /// staging — is an `err`, never `contended`.
    pub(crate) surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestWaitDeviceIdle {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}
