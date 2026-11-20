// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

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

    /// Slang source for the compute shader; the engine compiles it. Mutually
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
    /// The blob must retain its `OpName` decorations: bindings resolve by
    /// name, so a stripped blob is refused at registration. What the engine
    /// compiles from `source` keeps them by construction.
    #[serde(default)]
    pub(crate) spv_hex: String,
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
    /// kernel_id may appear twice in one batch: a recording binds a kernel
    /// through one descriptor set, so the second bind would retarget the
    /// dispatch already recorded against it. Refused with an `err` response.
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
