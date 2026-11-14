// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

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

    /// Slang source for the fragment stage. Mutually exclusive with
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

    /// Slang source for the vertex stage. Mutually exclusive with
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
