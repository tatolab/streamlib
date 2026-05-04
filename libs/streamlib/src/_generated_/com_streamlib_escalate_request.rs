// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Polyglot subprocess escalate-on-behalf request (subprocess → host)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum EscalateRequest {
    #[serde(rename = "acquire_image")]
    AcquireImage(EscalateRequestAcquireImage),

    #[serde(rename = "acquire_pixel_buffer")]
    AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer),

    #[serde(rename = "acquire_texture")]
    AcquireTexture(EscalateRequestAcquireTexture),

    #[serde(rename = "log")]
    Log(EscalateRequestLog),

    #[serde(rename = "register_compute_kernel")]
    RegisterComputeKernel(EscalateRequestRegisterComputeKernel),

    #[serde(rename = "register_graphics_kernel")]
    RegisterGraphicsKernel(EscalateRequestRegisterGraphicsKernel),

    #[serde(rename = "release_handle")]
    ReleaseHandle(EscalateRequestReleaseHandle),

    #[serde(rename = "run_compute_kernel")]
    RunComputeKernel(EscalateRequestRunComputeKernel),

    #[serde(rename = "run_cpu_readback_copy")]
    RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy),

    #[serde(rename = "run_graphics_draw")]
    RunGraphicsDraw(EscalateRequestRunGraphicsDraw),

    #[serde(rename = "try_run_cpu_readback_copy")]
    TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestAcquireImage {
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
    #[serde(rename = "format")]
    pub format: String,

    /// Pixel height of the image.
    #[serde(rename = "height")]
    pub height: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Pixel width of the image.
    #[serde(rename = "width")]
    pub width: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestAcquirePixelBuffer {
    /// Pixel format identifier (e.g. bgra32, nv12_video_range, gray8).
    #[serde(rename = "format")]
    pub format: String,

    /// Pixel height of the buffer.
    #[serde(rename = "height")]
    pub height: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Pixel width of the buffer.
    #[serde(rename = "width")]
    pub width: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestAcquireTexture {
    /// Texture format identifier. Lowercase snake-case names: rgba8_unorm,
    /// rgba8_unorm_srgb, bgra8_unorm, bgra8_unorm_srgb, rgba16_float,
    /// rgba32_float, nv12.
    #[serde(rename = "format")]
    pub format: String,

    /// Pixel height of the texture.
    #[serde(rename = "height")]
    pub height: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Usage flags the texture must support. Non-empty array of lowercase
    /// snake-case tokens drawn from: copy_src, copy_dst, texture_binding,
    /// storage_binding, render_attachment. Host validates — unknown tokens
    /// return an error response.
    #[serde(rename = "usage")]
    pub usage: Vec<String>,

    /// Pixel width of the texture.
    #[serde(rename = "width")]
    pub width: u32,
}

/// Severity level of the record. Maps 1:1 onto tracing::Level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestLogLevel {
    #[serde(rename = "debug")]
    #[default]
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

/// Origin runtime of the record. Always "python" or "deno" on the wire — Rust
/// never routes through escalate; Rust call sites hit `tracing::*!()` directly
/// on the host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestLogSource {
    #[serde(rename = "deno")]
    #[default]
    Deno,

    #[serde(rename = "python")]
    Python,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestLog {
    /// User-supplied structured fields. Copied flat onto the emitted
    /// RuntimeLogEvent's `attrs` map — not nested under an `attrs.key` path in
    /// the JSONL.
    #[serde(rename = "attrs")]
    pub attrs: HashMap<String, Option<Value>>,

    /// Interceptor channel when `intercepted: true`. Conventional values:
    /// "stdout", "stderr", "console.log", "logging", "fd1", "fd2". Null when
    /// `intercepted: false`.
    #[serde(rename = "channel")]
    pub channel: Option<String>,

    /// True when the record was captured from subprocess stdout/stderr,
    /// console.log, root logging handler, or a raw fd write, rather than a
    /// direct `streamlib.log.*` call.
    #[serde(rename = "intercepted")]
    pub intercepted: bool,

    /// Severity level of the record. Maps 1:1 onto tracing::Level.
    #[serde(rename = "level")]
    pub level: EscalateRequestLogLevel,

    /// Primary human-readable message.
    #[serde(rename = "message")]
    pub message: String,

    /// Pipeline identifier. Null for runtime-level records.
    #[serde(rename = "pipeline_id")]
    pub pipeline_id: Option<String>,

    /// Processor identifier. Null outside a processor.
    #[serde(rename = "processor_id")]
    pub processor_id: Option<String>,

    /// Origin runtime of the record. Always "python" or "deno" on the wire —
    /// Rust never routes through escalate; Rust call sites hit `tracing::*!()`
    /// directly on the host.
    #[serde(rename = "source")]
    pub source: EscalateRequestLogSource,

    /// Subprocess-monotonic sequence number (uint64 as string — JTD has no
    /// native u64). Escape hatch for recovering subprocess-local order within
    /// a single source. Not authoritative across sources — use `host_ts` for
    /// merged-stream ordering.
    #[serde(rename = "source_seq")]
    pub source_seq: String,

    /// Subprocess wall-clock timestamp ISO8601 (advisory). Never used for
    /// ordering; the host stamps `host_ts` on receipt as the authoritative
    /// sort key.
    #[serde(rename = "source_ts")]
    pub source_ts: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRegisterComputeKernel {
    /// Push-constant range size in bytes. 0 if the shader uses no push
    /// constants. The host validates this against the shader's reflected push-
    /// constant range and rejects mismatches with an `err` response.
    #[serde(rename = "push_constant_size")]
    pub push_constant_size: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Compiled SPIR-V bytecode for the compute shader, encoded as lowercase
    /// hex (no `0x` prefix, no whitespace). The host parses the bytes back,
    /// derives the binding shape from `rspirv-reflect`, and constructs a
    /// `VulkanComputeKernel` via `GpuContext::create_compute_kernel`.
    /// Re-registering identical SPIR-V is a host-side cache hit keyed by SHA-
    /// 256(spv_bytes) — no re-reflection, no fresh pipeline. The returned
    /// `kernel_id` is the same.
    /// The host's `VulkanComputeKernel` also persists driver- compiled pipeline
    /// state to `<XDG_CACHE_HOME>/streamlib/ pipeline-cache/<spirv_hash>.bin`,
    /// so first-inference latency after a host process restart is fast on user-
    /// registered ML kernels.
    #[serde(rename = "spv_hex")]
    pub spv_hex: String,
}

/// Resource kind for this binding slot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelBindingKind {
    #[serde(rename = "sampled_texture")]
    #[default]
    SampledTexture,

    #[serde(rename = "storage_buffer")]
    StorageBuffer,

    #[serde(rename = "storage_image")]
    StorageImage,

    #[serde(rename = "uniform_buffer")]
    UniformBuffer,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRegisterGraphicsKernelBinding {
    #[serde(rename = "binding")]
    pub binding: u32,

    /// Resource kind for this binding slot.
    #[serde(rename = "kind")]
    pub kind: EscalateRequestRegisterGraphicsKernelBindingKind,

    /// Bitmask of stages the binding is visible to. `1 = VERTEX`, `2 =
    /// FRAGMENT`, `3 = VERTEX_FRAGMENT`.
    #[serde(rename = "stages")]
    pub stages: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp {
    #[serde(rename = "add")]
    #[default]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp {
    #[serde(rename = "add")]
    #[default]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor {
    #[serde(rename = "constant_alpha")]
    #[default]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor {
    #[serde(rename = "constant_alpha")]
    #[default]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor {
    #[serde(rename = "constant_alpha")]
    #[default]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor {
    #[serde(rename = "constant_alpha")]
    #[default]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp {
    #[serde(rename = "always")]
    #[default]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState {
    #[serde(rename = "none")]
    #[default]
    None,

    #[serde(rename = "viewport_scissor")]
    ViewportScissor,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode {
    #[serde(rename = "back")]
    #[default]
    Back,

    #[serde(rename = "front")]
    Front,

    #[serde(rename = "front_and_back")]
    FrontAndBack,

    #[serde(rename = "none")]
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace {
    #[serde(rename = "clockwise")]
    #[default]
    Clockwise,

    #[serde(rename = "counter_clockwise")]
    CounterClockwise,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode {
    #[serde(rename = "fill")]
    #[default]
    Fill,

    #[serde(rename = "line")]
    Line,

    #[serde(rename = "point")]
    Point,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateTopology {
    #[serde(rename = "line_list")]
    #[default]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat {
    #[serde(rename = "r32_float")]
    #[default]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute {
    #[serde(rename = "binding")]
    pub binding: u32,

    #[serde(rename = "format")]
    pub format: EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat,

    #[serde(rename = "location")]
    pub location: u32,

    #[serde(rename = "offset")]
    pub offset: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate {
    #[serde(rename = "instance")]
    #[default]
    Instance,

    #[serde(rename = "vertex")]
    Vertex,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding {
    #[serde(rename = "binding")]
    pub binding: u32,

    #[serde(rename = "input_rate")]
    pub input_rate: EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate,

    #[serde(rename = "stride")]
    pub stride: u32,
}

/// Depth attachment format. Absent disables depth attachments — the
/// depth_stencil flags must be consistent (`depth_stencil_enabled = false` when
/// this is absent).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat {
    #[serde(rename = "d16_unorm")]
    #[default]
    D16Unorm,

    #[serde(rename = "d24_unorm_s8_uint")]
    D24UnormS8Uint,

    #[serde(rename = "d32_sfloat")]
    D32Sfloat,
}

/// Fixed-function pipeline state plus attachment formats for the graphics
/// pipeline. Mirrors the host `GraphicsPipelineState` shape; unsupported
/// combinations (multi-attachment color blend, MSAA samples > 1, etc.) are
/// rejected with an `err` response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRegisterGraphicsKernelPipelineState {
    /// Color attachment texture formats (lowercase snake-case names matching
    /// `acquire_texture.format`). v1 supports a single color attachment; arrays
    /// of length other than 1 are rejected.
    #[serde(rename = "attachment_color_formats")]
    pub attachment_color_formats: Vec<String>,

    #[serde(rename = "color_blend_alpha_op")]
    pub color_blend_alpha_op: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,

    #[serde(rename = "color_blend_color_op")]
    pub color_blend_color_op: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,

    #[serde(rename = "color_blend_dst_alpha_factor")]
    pub color_blend_dst_alpha_factor: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,

    #[serde(rename = "color_blend_dst_color_factor")]
    pub color_blend_dst_color_factor: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,

    #[serde(rename = "color_blend_enabled")]
    pub color_blend_enabled: bool,

    #[serde(rename = "color_blend_src_alpha_factor")]
    pub color_blend_src_alpha_factor: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,

    /// Blend factor. Ignored when `color_blend_enabled` is false; carry a valid
    /// value (e.g. `one`) regardless.
    #[serde(rename = "color_blend_src_color_factor")]
    pub color_blend_src_color_factor: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,

    /// Color write mask bits — `1=R`, `2=G`, `4=B`, `8=A`. `15` (`0b1111`)
    /// writes RGBA. Used both when blending is disabled and as the blend
    /// attachment's `color_write_mask` when enabled.
    #[serde(rename = "color_write_mask")]
    pub color_write_mask: u32,

    /// Depth compare op. Ignored when `depth_stencil_enabled` is false; the
    /// wire field must still carry a valid value (use `always` as the default
    /// placeholder when disabled).
    #[serde(rename = "depth_compare_op")]
    pub depth_compare_op: EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp,

    #[serde(rename = "depth_stencil_enabled")]
    pub depth_stencil_enabled: bool,

    #[serde(rename = "depth_write")]
    pub depth_write: bool,

    /// Which pipeline state is set dynamically per draw vs baked into the
    /// pipeline at creation. `none` bakes a default 1×1 viewport (offscreen
    /// fixed-size only); `viewport_scissor` lets the same pipeline serve
    /// varying extents.
    #[serde(rename = "dynamic_state")]
    pub dynamic_state: EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState,

    /// MSAA sample count. Only `1` is supported in v1; any other value returns
    /// an `err` response.
    #[serde(rename = "multisample_samples")]
    pub multisample_samples: u32,

    #[serde(rename = "rasterization_cull_mode")]
    pub rasterization_cull_mode: EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode,

    #[serde(rename = "rasterization_front_face")]
    pub rasterization_front_face: EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace,

    #[serde(rename = "rasterization_line_width")]
    pub rasterization_line_width: f32,

    #[serde(rename = "rasterization_polygon_mode")]
    pub rasterization_polygon_mode: EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode,

    #[serde(rename = "topology")]
    pub topology: EscalateRequestRegisterGraphicsKernelPipelineStateTopology,

    /// Vertex attributes pulled from the bindings. Must be empty when
    /// `vertex_input_bindings` is empty.
    #[serde(rename = "vertex_input_attributes")]
    pub vertex_input_attributes: Vec<EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute>,

    /// Vertex buffer binding slots — stride and step rate per binding. Empty
    /// array selects the `VertexInputState::None` (gl_VertexIndex-driven)
    /// shape; non-empty selects `VertexInputState::Buffers` with the given
    /// bindings + attributes.
    #[serde(rename = "vertex_input_bindings")]
    pub vertex_input_bindings: Vec<EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding>,

    /// Depth attachment format. Absent disables depth attachments — the
    /// depth_stencil flags must be consistent (`depth_stencil_enabled = false`
    /// when this is absent).
    #[serde(rename = "attachment_depth_format")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_depth_format: Option<EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRegisterGraphicsKernel {
    /// Descriptor-set-0 bindings the host pipeline declares. Validated against
    /// `rspirv-reflect` of the supplied SPIR-V at register time — mismatches
    /// return an `err` response. Empty array means no bindings.
    #[serde(rename = "bindings")]
    pub bindings: Vec<EscalateRequestRegisterGraphicsKernelBinding>,

    /// Depth of the descriptor-set ring. Render-loop callers pass `frame_index
    /// ∈ [0, descriptor_sets_in_flight)` per draw. Must be ≥ 1.
    #[serde(rename = "descriptor_sets_in_flight")]
    pub descriptor_sets_in_flight: u32,

    /// Entry-point name for the fragment stage. Empty string is normalized to
    /// `"main"` host-side.
    #[serde(rename = "fragment_entry_point")]
    pub fragment_entry_point: String,

    /// Compiled SPIR-V bytecode for the fragment stage, encoded as lowercase
    /// hex. Today exactly one fragment stage is required (matching the host
    /// kernel's v1 contract).
    #[serde(rename = "fragment_spv_hex")]
    pub fragment_spv_hex: String,

    /// Human-readable label used in error messages and tracing on the host.
    /// Echoed in `kernel_id` derivation only via its bytes — purely diagnostic.
    #[serde(rename = "label")]
    pub label: String,

    /// Fixed-function pipeline state plus attachment formats for the graphics
    /// pipeline. Mirrors the host `GraphicsPipelineState` shape; unsupported
    /// combinations (multi-attachment color blend, MSAA samples > 1, etc.) are
    /// rejected with an `err` response.
    #[serde(rename = "pipeline_state")]
    pub pipeline_state: EscalateRequestRegisterGraphicsKernelPipelineState,

    /// Push-constant range size in bytes, validated against the merged shader
    /// reflection. Set 0 if the shaders use no push constants.
    #[serde(rename = "push_constant_size")]
    pub push_constant_size: u32,

    /// Bitmask of stages the push-constant range is visible to. `1 = VERTEX`,
    /// `2 = FRAGMENT`. Ignored when `push_constant_size == 0`.
    #[serde(rename = "push_constant_stages")]
    pub push_constant_stages: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Entry-point name for the vertex stage. Empty string is normalized to
    /// `"main"` host-side.
    #[serde(rename = "vertex_entry_point")]
    pub vertex_entry_point: String,

    /// Compiled SPIR-V bytecode for the vertex stage, encoded as lowercase
    /// hex (no `0x` prefix, no whitespace). Today exactly one vertex stage
    /// is required (the host kernel rejects zero or multiple vertex stages).
    /// Geometry / tessellation / mesh / task stages are not yet supported.
    #[serde(rename = "vertex_spv_hex")]
    pub vertex_spv_hex: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestReleaseHandle {
    /// Opaque handle ID previously returned by acquire_*.
    #[serde(rename = "handle_id")]
    pub handle_id: String,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunComputeKernel {
    /// vkCmdDispatch groupCountX.
    #[serde(rename = "group_count_x")]
    pub group_count_x: u32,

    /// vkCmdDispatch groupCountY.
    #[serde(rename = "group_count_y")]
    pub group_count_y: u32,

    /// vkCmdDispatch groupCountZ.
    #[serde(rename = "group_count_z")]
    pub group_count_z: u32,

    /// Handle returned by a prior `register_compute_kernel` response. The
    /// host looks up the cached `Arc<VulkanComputeKernel>` and dispatches
    /// against it. Dispatching with an unrecognized kernel_id returns an `err`
    /// response.
    #[serde(rename = "kernel_id")]
    pub kernel_id: String,

    /// Push-constant payload for this dispatch, encoded as lowercase hex.
    /// Length in bytes (after hex decoding) must equal the kernel's declared
    /// `push_constant_size`. Empty string when the kernel has no push
    /// constants.
    #[serde(rename = "push_constants_hex")]
    pub push_constants_hex: String,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// UUID string of a render-target surface previously registered with
    /// the surface-share service via `register_texture`. The host bridge
    /// holds an application-provided UUID→`StreamTexture` map (populated
    /// in `install_setup_hook`) and binds the looked-up `VkImage` as a
    /// storage_image at slot 0 (the single-output convention enforced for v1
    /// — multi-binding kernels are a future extension). UUID rather than u64
    /// so the host can resolve the surface without subprocess-side counter
    /// coordination.
    #[serde(rename = "surface_uuid")]
    pub surface_uuid: String,
}

/// Which copy direction to run on the host. `image_to_buffer` runs
/// `vkCmdCopyImageToBuffer` (image → staging) at acquire time;
/// `buffer_to_image` runs the reverse at write release. The host signals a new
/// value on the surface's timeline at end-of-submit; the subprocess waits on
/// the timeline (through its imported `ConsumerVulkanTimelineSemaphore`) before
/// reading or releasing. No FDs travel on the wire — only the timeline value
/// the host signaled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRunCpuReadbackCopyDirection {
    #[serde(rename = "buffer_to_image")]
    #[default]
    BufferToImage,

    #[serde(rename = "image_to_buffer")]
    ImageToBuffer,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunCpuReadbackCopy {
    /// Which copy direction to run on the host. `image_to_buffer` runs
    /// `vkCmdCopyImageToBuffer` (image → staging) at acquire time;
    /// `buffer_to_image` runs the reverse at write release. The host
    /// signals a new value on the surface's timeline at end-of-submit;
    /// the subprocess waits on the timeline (through its imported
    /// `ConsumerVulkanTimelineSemaphore`) before reading or releasing. No FDs
    /// travel on the wire — only the timeline value the host signaled.
    #[serde(rename = "direction")]
    pub direction: EscalateRequestRunCpuReadbackCopyDirection,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Host-assigned surface id (the u64 carried by `StreamlibSurface::id`)
    /// of a surface previously registered with the host's cpu-readback adapter
    /// and whose staging buffer + timeline were registered with the surface-
    /// share service via `register_pixel_buffer_with_timeline`. The subprocess
    /// imported them once at registration time through `streamlib-consumer-
    /// rhi`'s `ConsumerVulkanPixelBuffer` / `ConsumerVulkanTimelineSemaphore`.
    /// JTD has no native u64 — the wire form is the decimal string
    /// representation, parsed back into u64 by the host before dispatch.
    #[serde(rename = "surface_id")]
    pub surface_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRunGraphicsDrawBindingKind {
    #[serde(rename = "sampled_texture")]
    #[default]
    SampledTexture,

    #[serde(rename = "storage_buffer")]
    StorageBuffer,

    #[serde(rename = "storage_image")]
    StorageImage,

    #[serde(rename = "uniform_buffer")]
    UniformBuffer,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunGraphicsDrawBinding {
    #[serde(rename = "binding")]
    pub binding: u32,

    #[serde(rename = "kind")]
    pub kind: EscalateRequestRunGraphicsDrawBindingKind,

    #[serde(rename = "surface_uuid")]
    pub surface_uuid: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRunGraphicsDrawDrawKind {
    #[serde(rename = "draw")]
    #[default]
    Draw,

    #[serde(rename = "draw_indexed")]
    DrawIndexed,
}

/// Draw call. `kind = "draw"` selects non-indexed (`vertex_count`-driven),
/// `kind = "draw_indexed"` requires `index_buffer` to be set and uses
/// `index_count` / `first_index` / `vertex_offset`. Fields not used by the
/// selected kind are ignored host-side; subprocesses should still send valid
/// placeholder values (zero is fine) to keep the wire shape regular.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunGraphicsDrawDraw {
    #[serde(rename = "first_index")]
    pub first_index: u32,

    #[serde(rename = "first_instance")]
    pub first_instance: u32,

    #[serde(rename = "first_vertex")]
    pub first_vertex: u32,

    #[serde(rename = "index_count")]
    pub index_count: u32,

    #[serde(rename = "instance_count")]
    pub instance_count: u32,

    #[serde(rename = "kind")]
    pub kind: EscalateRequestRunGraphicsDrawDrawKind,

    #[serde(rename = "vertex_count")]
    pub vertex_count: u32,

    #[serde(rename = "vertex_offset")]
    pub vertex_offset: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunGraphicsDrawVertexBuffer {
    #[serde(rename = "binding")]
    pub binding: u32,

    #[serde(rename = "offset")]
    pub offset: String,

    #[serde(rename = "surface_uuid")]
    pub surface_uuid: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestRunGraphicsDrawIndexBufferIndexType {
    #[serde(rename = "uint16")]
    #[default]
    Uint16,

    #[serde(rename = "uint32")]
    Uint32,
}

/// Required when `draw.kind == "draw_indexed"`, must be absent otherwise.
/// `surface_uuid` resolves to an `RhiPixelBuffer`; `offset` is the byte offset
/// into it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunGraphicsDrawIndexBuffer {
    #[serde(rename = "index_type")]
    pub index_type: EscalateRequestRunGraphicsDrawIndexBufferIndexType,

    #[serde(rename = "offset")]
    pub offset: String,

    #[serde(rename = "surface_uuid")]
    pub surface_uuid: String,
}

/// Dynamic scissor rect for this draw. Required when the kernel declared
/// `dynamic_state = "viewport_scissor"`; ignored otherwise.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunGraphicsDrawScissor {
    #[serde(rename = "height")]
    pub height: u32,

    #[serde(rename = "width")]
    pub width: u32,

    #[serde(rename = "x")]
    pub x: i32,

    #[serde(rename = "y")]
    pub y: i32,
}

/// Dynamic viewport for this draw. Required when the kernel's pipeline state
/// declared `dynamic_state = "viewport_scissor"`; ignored otherwise.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunGraphicsDrawViewport {
    #[serde(rename = "height")]
    pub height: f32,

    #[serde(rename = "max_depth")]
    pub max_depth: f32,

    #[serde(rename = "min_depth")]
    pub min_depth: f32,

    #[serde(rename = "width")]
    pub width: f32,

    #[serde(rename = "x")]
    pub x: f32,

    #[serde(rename = "y")]
    pub y: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestRunGraphicsDraw {
    /// Per-draw bindings — each slot's `surface_uuid` must resolve through
    /// the host bridge's UUID → resource map. `kind` must match the binding's
    /// declared kind from register time.
    #[serde(rename = "bindings")]
    pub bindings: Vec<EscalateRequestRunGraphicsDrawBinding>,

    /// UUIDs of color attachment textures. v1 requires exactly one entry —
    /// multi-attachment is a future extension. Each UUID must resolve to a
    /// host-side `StreamTexture` registered as a render target.
    #[serde(rename = "color_target_uuids")]
    pub color_target_uuids: Vec<String>,

    /// Draw call. `kind = "draw"` selects non-indexed (`vertex_count`-driven),
    /// `kind = "draw_indexed"` requires `index_buffer` to be set and uses
    /// `index_count` / `first_index` / `vertex_offset`. Fields not used by
    /// the selected kind are ignored host-side; subprocesses should still send
    /// valid placeholder values (zero is fine) to keep the wire shape regular.
    #[serde(rename = "draw")]
    pub draw: EscalateRequestRunGraphicsDrawDraw,

    /// Render-area height in pixels.
    #[serde(rename = "extent_height")]
    pub extent_height: u32,

    /// Render-area width in pixels.
    #[serde(rename = "extent_width")]
    pub extent_width: u32,

    /// Slot in the kernel's descriptor-set ring. Must satisfy `frame_index
    /// < descriptor_sets_in_flight` declared at register time. Render-loop
    /// callers cycle this through `MAX_FRAMES_IN_FLIGHT` so concurrent frames
    /// don't scribble each other's bindings.
    #[serde(rename = "frame_index")]
    pub frame_index: u32,

    /// Handle returned by a prior `register_graphics_kernel` response. The
    /// host looks up the cached `Arc<VulkanGraphicsKernel>` and dispatches
    /// against it. Dispatching with an unrecognized kernel_id returns an `err`
    /// response.
    #[serde(rename = "kernel_id")]
    pub kernel_id: String,

    /// Push-constant payload for this draw, lowercase hex. Must decode to
    /// exactly the kernel's declared `push_constant_size` (or empty if zero).
    #[serde(rename = "push_constants_hex")]
    pub push_constants_hex: String,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Per-draw vertex buffer bindings. Each entry's `surface_uuid` must
    /// resolve to a host-side `RhiPixelBuffer`. `offset` is the byte offset
    /// into the buffer where vertex data starts (decimal-encoded u64 — JTD has
    /// no native u64). Empty for vertex-fabricating shaders (`gl_VertexIndex`
    /// patterns).
    #[serde(rename = "vertex_buffers")]
    pub vertex_buffers: Vec<EscalateRequestRunGraphicsDrawVertexBuffer>,

    /// UUID of a depth attachment texture. Reserved for future use — v1 rejects
    /// depth attachments with an `err` response.
    #[serde(rename = "depth_target_uuid")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth_target_uuid: Option<String>,

    /// Required when `draw.kind == "draw_indexed"`, must be absent otherwise.
    /// `surface_uuid` resolves to an `RhiPixelBuffer`; `offset` is the byte
    /// offset into it.
    #[serde(rename = "index_buffer")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_buffer: Option<EscalateRequestRunGraphicsDrawIndexBuffer>,

    /// Dynamic scissor rect for this draw. Required when the kernel declared
    /// `dynamic_state = "viewport_scissor"`; ignored otherwise.
    #[serde(rename = "scissor")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scissor: Option<EscalateRequestRunGraphicsDrawScissor>,

    /// Dynamic viewport for this draw. Required when the kernel's pipeline
    /// state declared `dynamic_state = "viewport_scissor"`; ignored otherwise.
    #[serde(rename = "viewport")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<EscalateRequestRunGraphicsDrawViewport>,
}

/// Same shape as `run_cpu_readback_copy.direction`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestTryRunCpuReadbackCopyDirection {
    #[serde(rename = "buffer_to_image")]
    #[default]
    BufferToImage,

    #[serde(rename = "image_to_buffer")]
    ImageToBuffer,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestTryRunCpuReadbackCopy {
    /// Same shape as `run_cpu_readback_copy.direction`.
    #[serde(rename = "direction")]
    pub direction: EscalateRequestTryRunCpuReadbackCopyDirection,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Same shape as `run_cpu_readback_copy.surface_id`. The host returns a
    /// [`contended`] response (no timeline value, no copy executed) when its
    /// registry would have blocked instead of performing the copy. Subprocess
    /// customers use this to skip a frame instead of stalling their thread
    /// runner.
    #[serde(rename = "surface_id")]
    pub surface_id: String,
}
