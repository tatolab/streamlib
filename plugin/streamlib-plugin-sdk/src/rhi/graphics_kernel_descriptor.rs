// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor types for the graphics-kernel RHI abstraction.
//!
//! Pure-data twins of the engine's `core::rhi` graphics descriptor shapes
//! (the engine's `graphics_kernel.rs` additionally hosts the
//! `rspirv-reflect`-driven `derive_bindings_from_spirv_multistage` helper,
//! which is host-only; the SDK carries only the byte-shaped declaration
//! types a plugin hands to `create_graphics_kernel`). The kernel author
//! declares the stages, binding shape, and fixed-function pipeline state
//! once as data; the host reflects the SPIR-V at kernel creation and
//! validates the declaration matches.

use streamlib_consumer_rhi::TextureFormat;

/// Shader stages that contribute to a graphics pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphicsShaderStage {
    Vertex,
    Fragment,
}

/// Bitflags for which shader stages a binding or push-constant range is
/// visible to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsShaderStageFlags(u32);

impl GraphicsShaderStageFlags {
    pub const NONE: Self = Self(0);
    pub const VERTEX: Self = Self(0b01);
    pub const FRAGMENT: Self = Self(0b10);
    pub const VERTEX_FRAGMENT: Self = Self(0b11);

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl std::ops::BitOr for GraphicsShaderStageFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for GraphicsShaderStageFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// One stage of a graphics pipeline: SPIR-V blob plus stage classification.
#[derive(Debug, Clone, Copy)]
pub struct GraphicsStage<'a> {
    pub stage: GraphicsShaderStage,
    pub spv: &'a [u8],
    /// Entry point name. Defaults to `"main"`.
    pub entry_point: &'a str,
}

impl<'a> GraphicsStage<'a> {
    pub const fn vertex(spv: &'a [u8]) -> Self {
        Self {
            stage: GraphicsShaderStage::Vertex,
            spv,
            entry_point: "main",
        }
    }

    pub const fn fragment(spv: &'a [u8]) -> Self {
        Self {
            stage: GraphicsShaderStage::Fragment,
            spv,
            entry_point: "main",
        }
    }
}

/// Resource kind bound at a particular slot in a graphics kernel's
/// descriptor set 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsBindingKind {
    SampledTexture,
    StorageBuffer,
    UniformBuffer,
    StorageImage,
}

/// One binding declaration: (binding index, resource kind, visible stages).
#[derive(Debug, Clone, Copy)]
pub struct GraphicsBindingSpec {
    pub binding: u32,
    pub kind: GraphicsBindingKind,
    pub stages: GraphicsShaderStageFlags,
}

impl GraphicsBindingSpec {
    pub const fn sampled_texture(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::SampledTexture,
            stages,
        }
    }

    pub const fn storage_buffer(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::StorageBuffer,
            stages,
        }
    }

    pub const fn uniform_buffer(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::UniformBuffer,
            stages,
        }
    }

    pub const fn storage_image(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::StorageImage,
            stages,
        }
    }
}

/// Push-constant range declaration. Set `size = 0` to opt out.
#[derive(Debug, Clone, Copy)]
pub struct GraphicsPushConstants {
    pub size: u32,
    pub stages: GraphicsShaderStageFlags,
}

impl GraphicsPushConstants {
    pub const NONE: Self = Self {
        size: 0,
        stages: GraphicsShaderStageFlags::NONE,
    };
}

/// Primitive topology for assembled vertices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveTopology {
    PointList,
    LineList,
    LineStrip,
    TriangleList,
    TriangleStrip,
    TriangleFan,
}

/// Per-vertex format for a vertex attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAttributeFormat {
    R32Float,
    Rg32Float,
    Rgb32Float,
    Rgba32Float,
    R32Uint,
    Rg32Uint,
    Rgb32Uint,
    Rgba32Uint,
    R32Sint,
    Rg32Sint,
    Rgb32Sint,
    Rgba32Sint,
    Rgba8Unorm,
    Rgba8Snorm,
}

/// Whether a vertex input binding advances per-vertex or per-instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexInputRate {
    Vertex,
    Instance,
}

/// One vertex buffer binding slot: stride and step rate.
#[derive(Debug, Clone, Copy)]
pub struct VertexInputBinding {
    pub binding: u32,
    pub stride: u32,
    pub input_rate: VertexInputRate,
}

/// One vertex attribute pulled from a binding: shader location, source
/// binding, element format, byte offset within the vertex.
#[derive(Debug, Clone, Copy)]
pub struct VertexInputAttribute {
    pub location: u32,
    pub binding: u32,
    pub format: VertexAttributeFormat,
    pub offset: u32,
}

/// Vertex input state. `None` means the vertex shader fabricates positions
/// from `gl_VertexIndex` / `gl_InstanceIndex` (fullscreen-triangle pattern).
#[derive(Debug, Clone)]
pub enum VertexInputState {
    None,
    Buffers {
        bindings: Vec<VertexInputBinding>,
        attributes: Vec<VertexInputAttribute>,
    },
}

/// Polygon fill mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonMode {
    Fill,
    Line,
    Point,
}

/// Face-culling mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CullMode {
    None,
    Front,
    Back,
    FrontAndBack,
}

/// Front-face winding order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontFace {
    CounterClockwise,
    Clockwise,
}

/// Rasterization fixed-function state.
#[derive(Debug, Clone, Copy)]
pub struct RasterizationState {
    pub polygon_mode: PolygonMode,
    pub cull_mode: CullMode,
    pub front_face: FrontFace,
    pub line_width: f32,
}

impl Default for RasterizationState {
    fn default() -> Self {
        Self {
            polygon_mode: PolygonMode::Fill,
            cull_mode: CullMode::None,
            front_face: FrontFace::CounterClockwise,
            line_width: 1.0,
        }
    }
}

/// Multisample state. Only `samples = 1` is supported today.
#[derive(Debug, Clone, Copy)]
pub struct MultisampleState {
    pub samples: u32,
}

impl Default for MultisampleState {
    fn default() -> Self {
        Self { samples: 1 }
    }
}

/// Depth comparison op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthCompareOp {
    Never,
    Less,
    Equal,
    LessOrEqual,
    Greater,
    NotEqual,
    GreaterOrEqual,
    Always,
}

/// Depth/stencil test state. Stencil testing is not exposed today.
#[derive(Debug, Clone, Copy)]
pub enum DepthStencilState {
    Disabled,
    Enabled {
        depth_test: DepthCompareOp,
        depth_write: bool,
    },
}

/// Source/destination blend factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    DstColor,
    OneMinusDstColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    ConstantColor,
    OneMinusConstantColor,
    ConstantAlpha,
    OneMinusConstantAlpha,
    SrcAlphaSaturate,
}

/// Blend equation op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

/// Color write-mask flags for a color attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorWriteMask(u32);

impl ColorWriteMask {
    pub const R: Self = Self(0b0001);
    pub const G: Self = Self(0b0010);
    pub const B: Self = Self(0b0100);
    pub const A: Self = Self(0b1000);
    pub const RGBA: Self = Self(0b1111);

    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for ColorWriteMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Per-attachment color-blend description.
#[derive(Debug, Clone, Copy)]
pub struct ColorBlendAttachment {
    pub src_color_blend_factor: BlendFactor,
    pub dst_color_blend_factor: BlendFactor,
    pub color_blend_op: BlendOp,
    pub src_alpha_blend_factor: BlendFactor,
    pub dst_alpha_blend_factor: BlendFactor,
    pub alpha_blend_op: BlendOp,
    pub color_write_mask: ColorWriteMask,
}

impl ColorBlendAttachment {
    /// Standard alpha blend over the destination.
    pub const ALPHA_OVER: Self = Self {
        src_color_blend_factor: BlendFactor::SrcAlpha,
        dst_color_blend_factor: BlendFactor::OneMinusSrcAlpha,
        color_blend_op: BlendOp::Add,
        src_alpha_blend_factor: BlendFactor::One,
        dst_alpha_blend_factor: BlendFactor::OneMinusSrcAlpha,
        alpha_blend_op: BlendOp::Add,
        color_write_mask: ColorWriteMask::RGBA,
    };
}

/// Color blend state. Targets a single color attachment today.
#[derive(Debug, Clone, Copy)]
pub enum ColorBlendState {
    Disabled { color_write_mask: ColorWriteMask },
    Enabled(ColorBlendAttachment),
}

impl Default for ColorBlendState {
    fn default() -> Self {
        Self::Disabled {
            color_write_mask: ColorWriteMask::RGBA,
        }
    }
}

/// Depth attachment format for graphics-pipeline depth testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthFormat {
    D16Unorm,
    D32Sfloat,
    D24UnormS8Uint,
}

/// Color and depth attachment formats the graphics pipeline targets.
#[derive(Debug, Clone)]
pub struct AttachmentFormats {
    pub color: Vec<TextureFormat>,
    pub depth: Option<DepthFormat>,
}

impl AttachmentFormats {
    /// Single color attachment, no depth.
    pub fn color_only(format: TextureFormat) -> Self {
        Self {
            color: vec![format],
            depth: None,
        }
    }
}

/// Which pipeline state is set dynamically per-draw vs baked at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsDynamicState {
    None,
    ViewportScissor,
}

/// Complete fixed-function state plus attachment formats for a graphics
/// pipeline.
#[derive(Debug, Clone)]
pub struct GraphicsPipelineState {
    pub topology: PrimitiveTopology,
    pub vertex_input: VertexInputState,
    pub rasterization: RasterizationState,
    pub multisample: MultisampleState,
    pub depth_stencil: DepthStencilState,
    pub color_blend: ColorBlendState,
    pub attachment_formats: AttachmentFormats,
    pub dynamic_state: GraphicsDynamicState,
}

/// Graphics-kernel descriptor: stages + bindings + pipeline state +
/// descriptor-set ring depth.
///
/// Pass to [`crate::context::GpuContextFullAccess::create_graphics_kernel`].
/// The host reflects every stage's SPIR-V on creation, validates that
/// `bindings` matches the merged shader declaration, and rejects
/// mismatches loudly.
#[derive(Debug, Clone)]
pub struct GraphicsKernelDescriptor<'a> {
    /// Human-readable label used in error messages and tracing.
    pub label: &'a str,
    /// One entry per shader stage (vertex + fragment minimum).
    pub stages: &'a [GraphicsStage<'a>],
    /// Binding declarations for descriptor set 0.
    pub bindings: &'a [GraphicsBindingSpec],
    /// Push-constant declaration. [`GraphicsPushConstants::NONE`] if unused.
    pub push_constants: GraphicsPushConstants,
    /// Fixed-function pipeline state + attachment formats.
    pub pipeline_state: GraphicsPipelineState,
    /// Number of descriptor sets in the ring. Must be ≥ 1.
    pub descriptor_sets_in_flight: u32,
}

/// Type of indices for indexed draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexType {
    Uint16,
    Uint32,
}

/// Viewport rectangle for dynamic-viewport draws.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

impl Viewport {
    /// Convenience: full-extent viewport with depth range \[0, 1\].
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }
}

/// Scissor rectangle for dynamic-scissor draws.
#[derive(Debug, Clone, Copy)]
pub struct ScissorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl ScissorRect {
    /// Convenience: full-extent scissor anchored at the origin.
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

/// One non-indexed draw call.
#[derive(Debug, Clone, Copy)]
pub struct DrawCall {
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
    pub viewport: Option<Viewport>,
    pub scissor: Option<ScissorRect>,
}

/// One indexed draw call. Caller must have set an index buffer first.
#[derive(Debug, Clone, Copy)]
pub struct DrawIndexedCall {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub vertex_offset: i32,
    pub first_instance: u32,
    pub viewport: Option<Viewport>,
    pub scissor: Option<ScissorRect>,
}

/// One color attachment for
/// [`crate::rhi::VulkanGraphicsKernel::offscreen_render`].
pub struct OffscreenColorTarget<'a> {
    pub texture: &'a crate::rhi::Texture,
    /// `Some` clears the attachment to this RGBA value before drawing;
    /// `None` loads existing contents.
    pub clear_color: Option<[f32; 4]>,
}

/// Draw variant for
/// [`crate::rhi::VulkanGraphicsKernel::offscreen_render`].
pub enum OffscreenDraw {
    Draw(DrawCall),
    DrawIndexed(DrawIndexedCall),
}

// =============================================================================
// Plugin-ABI repr conversions
// =============================================================================

use streamlib_plugin_abi::{
    AttachmentFormatsRepr, BlendFactorRepr, BlendOpRepr, ColorBlendAttachmentRepr,
    ColorBlendStateKindRepr, ColorBlendStateRepr, CullModeRepr, DepthCompareOpRepr,
    DepthFormatRepr, DepthStencilStateKindRepr, DepthStencilStateRepr, FrontFaceRepr,
    GraphicsBindingKindRepr, GraphicsBindingSpecRepr, GraphicsDynamicStateRepr,
    GraphicsKernelDescriptorRepr, GraphicsPipelineStateRepr, GraphicsPushConstantsRepr,
    GraphicsShaderStageRepr, GraphicsStageRepr, MultisampleStateRepr, PolygonModeRepr,
    PrimitiveTopologyRepr, RasterizationStateRepr, VertexAttributeFormatRepr,
    VertexInputAttributeRepr, VertexInputBindingRepr, VertexInputRateRepr,
    VertexInputStateKindRepr, VertexInputStateRepr,
};

impl From<GraphicsShaderStage> for GraphicsShaderStageRepr {
    fn from(value: GraphicsShaderStage) -> Self {
        match value {
            GraphicsShaderStage::Vertex => Self::Vertex,
            GraphicsShaderStage::Fragment => Self::Fragment,
        }
    }
}

impl From<GraphicsBindingKind> for GraphicsBindingKindRepr {
    fn from(value: GraphicsBindingKind) -> Self {
        match value {
            GraphicsBindingKind::SampledTexture => Self::SampledTexture,
            GraphicsBindingKind::StorageBuffer => Self::StorageBuffer,
            GraphicsBindingKind::UniformBuffer => Self::UniformBuffer,
            GraphicsBindingKind::StorageImage => Self::StorageImage,
        }
    }
}

impl From<&GraphicsBindingSpec> for GraphicsBindingSpecRepr {
    fn from(value: &GraphicsBindingSpec) -> Self {
        Self {
            binding: value.binding,
            kind: GraphicsBindingKindRepr::from(value.kind) as u32,
            stages: value.stages.bits(),
            _reserved_padding: 0,
        }
    }
}

impl From<GraphicsPushConstants> for GraphicsPushConstantsRepr {
    fn from(value: GraphicsPushConstants) -> Self {
        Self {
            size: value.size,
            stages: value.stages.bits(),
        }
    }
}

impl From<PrimitiveTopology> for PrimitiveTopologyRepr {
    fn from(value: PrimitiveTopology) -> Self {
        match value {
            PrimitiveTopology::PointList => Self::PointList,
            PrimitiveTopology::LineList => Self::LineList,
            PrimitiveTopology::LineStrip => Self::LineStrip,
            PrimitiveTopology::TriangleList => Self::TriangleList,
            PrimitiveTopology::TriangleStrip => Self::TriangleStrip,
            PrimitiveTopology::TriangleFan => Self::TriangleFan,
        }
    }
}

impl From<VertexAttributeFormat> for VertexAttributeFormatRepr {
    fn from(value: VertexAttributeFormat) -> Self {
        match value {
            VertexAttributeFormat::R32Float => Self::R32Float,
            VertexAttributeFormat::Rg32Float => Self::Rg32Float,
            VertexAttributeFormat::Rgb32Float => Self::Rgb32Float,
            VertexAttributeFormat::Rgba32Float => Self::Rgba32Float,
            VertexAttributeFormat::R32Uint => Self::R32Uint,
            VertexAttributeFormat::Rg32Uint => Self::Rg32Uint,
            VertexAttributeFormat::Rgb32Uint => Self::Rgb32Uint,
            VertexAttributeFormat::Rgba32Uint => Self::Rgba32Uint,
            VertexAttributeFormat::R32Sint => Self::R32Sint,
            VertexAttributeFormat::Rg32Sint => Self::Rg32Sint,
            VertexAttributeFormat::Rgb32Sint => Self::Rgb32Sint,
            VertexAttributeFormat::Rgba32Sint => Self::Rgba32Sint,
            VertexAttributeFormat::Rgba8Unorm => Self::Rgba8Unorm,
            VertexAttributeFormat::Rgba8Snorm => Self::Rgba8Snorm,
        }
    }
}

impl From<VertexInputRate> for VertexInputRateRepr {
    fn from(value: VertexInputRate) -> Self {
        match value {
            VertexInputRate::Vertex => Self::Vertex,
            VertexInputRate::Instance => Self::Instance,
        }
    }
}

impl From<&VertexInputBinding> for VertexInputBindingRepr {
    fn from(value: &VertexInputBinding) -> Self {
        Self {
            binding: value.binding,
            stride: value.stride,
            input_rate: VertexInputRateRepr::from(value.input_rate) as u32,
            _reserved_padding: 0,
        }
    }
}

impl From<&VertexInputAttribute> for VertexInputAttributeRepr {
    fn from(value: &VertexInputAttribute) -> Self {
        Self {
            location: value.location,
            binding: value.binding,
            format: VertexAttributeFormatRepr::from(value.format) as u32,
            offset: value.offset,
        }
    }
}

impl From<PolygonMode> for PolygonModeRepr {
    fn from(value: PolygonMode) -> Self {
        match value {
            PolygonMode::Fill => Self::Fill,
            PolygonMode::Line => Self::Line,
            PolygonMode::Point => Self::Point,
        }
    }
}

impl From<CullMode> for CullModeRepr {
    fn from(value: CullMode) -> Self {
        match value {
            CullMode::None => Self::None,
            CullMode::Front => Self::Front,
            CullMode::Back => Self::Back,
            CullMode::FrontAndBack => Self::FrontAndBack,
        }
    }
}

impl From<FrontFace> for FrontFaceRepr {
    fn from(value: FrontFace) -> Self {
        match value {
            FrontFace::CounterClockwise => Self::CounterClockwise,
            FrontFace::Clockwise => Self::Clockwise,
        }
    }
}

impl From<RasterizationState> for RasterizationStateRepr {
    fn from(value: RasterizationState) -> Self {
        Self {
            polygon_mode: PolygonModeRepr::from(value.polygon_mode) as u32,
            cull_mode: CullModeRepr::from(value.cull_mode) as u32,
            front_face: FrontFaceRepr::from(value.front_face) as u32,
            line_width: value.line_width,
        }
    }
}

impl From<MultisampleState> for MultisampleStateRepr {
    fn from(value: MultisampleState) -> Self {
        Self {
            samples: value.samples,
            _reserved_padding: 0,
        }
    }
}

impl From<DepthCompareOp> for DepthCompareOpRepr {
    fn from(value: DepthCompareOp) -> Self {
        match value {
            DepthCompareOp::Never => Self::Never,
            DepthCompareOp::Less => Self::Less,
            DepthCompareOp::Equal => Self::Equal,
            DepthCompareOp::LessOrEqual => Self::LessOrEqual,
            DepthCompareOp::Greater => Self::Greater,
            DepthCompareOp::NotEqual => Self::NotEqual,
            DepthCompareOp::GreaterOrEqual => Self::GreaterOrEqual,
            DepthCompareOp::Always => Self::Always,
        }
    }
}

impl From<DepthStencilState> for DepthStencilStateRepr {
    fn from(value: DepthStencilState) -> Self {
        match value {
            DepthStencilState::Disabled => Self {
                kind: DepthStencilStateKindRepr::Disabled as u32,
                depth_test: 0,
                depth_write: 0,
                _reserved_padding: 0,
            },
            DepthStencilState::Enabled {
                depth_test,
                depth_write,
            } => Self {
                kind: DepthStencilStateKindRepr::Enabled as u32,
                depth_test: DepthCompareOpRepr::from(depth_test) as u32,
                depth_write: depth_write as u32,
                _reserved_padding: 0,
            },
        }
    }
}

impl From<BlendFactor> for BlendFactorRepr {
    fn from(value: BlendFactor) -> Self {
        match value {
            BlendFactor::Zero => Self::Zero,
            BlendFactor::One => Self::One,
            BlendFactor::SrcColor => Self::SrcColor,
            BlendFactor::OneMinusSrcColor => Self::OneMinusSrcColor,
            BlendFactor::DstColor => Self::DstColor,
            BlendFactor::OneMinusDstColor => Self::OneMinusDstColor,
            BlendFactor::SrcAlpha => Self::SrcAlpha,
            BlendFactor::OneMinusSrcAlpha => Self::OneMinusSrcAlpha,
            BlendFactor::DstAlpha => Self::DstAlpha,
            BlendFactor::OneMinusDstAlpha => Self::OneMinusDstAlpha,
            BlendFactor::ConstantColor => Self::ConstantColor,
            BlendFactor::OneMinusConstantColor => Self::OneMinusConstantColor,
            BlendFactor::ConstantAlpha => Self::ConstantAlpha,
            BlendFactor::OneMinusConstantAlpha => Self::OneMinusConstantAlpha,
            BlendFactor::SrcAlphaSaturate => Self::SrcAlphaSaturate,
        }
    }
}

impl From<BlendOp> for BlendOpRepr {
    fn from(value: BlendOp) -> Self {
        match value {
            BlendOp::Add => Self::Add,
            BlendOp::Subtract => Self::Subtract,
            BlendOp::ReverseSubtract => Self::ReverseSubtract,
            BlendOp::Min => Self::Min,
            BlendOp::Max => Self::Max,
        }
    }
}

impl From<ColorBlendAttachment> for ColorBlendAttachmentRepr {
    fn from(value: ColorBlendAttachment) -> Self {
        Self {
            src_color_blend_factor: BlendFactorRepr::from(value.src_color_blend_factor) as u32,
            dst_color_blend_factor: BlendFactorRepr::from(value.dst_color_blend_factor) as u32,
            color_blend_op: BlendOpRepr::from(value.color_blend_op) as u32,
            src_alpha_blend_factor: BlendFactorRepr::from(value.src_alpha_blend_factor) as u32,
            dst_alpha_blend_factor: BlendFactorRepr::from(value.dst_alpha_blend_factor) as u32,
            alpha_blend_op: BlendOpRepr::from(value.alpha_blend_op) as u32,
            color_write_mask: value.color_write_mask.bits(),
            _reserved_padding: 0,
        }
    }
}

impl From<ColorBlendState> for ColorBlendStateRepr {
    fn from(value: ColorBlendState) -> Self {
        match value {
            ColorBlendState::Disabled { color_write_mask } => Self {
                kind: ColorBlendStateKindRepr::Disabled as u32,
                color_write_mask: color_write_mask.bits(),
                attachment: ColorBlendAttachmentRepr {
                    src_color_blend_factor: 0,
                    dst_color_blend_factor: 0,
                    color_blend_op: 0,
                    src_alpha_blend_factor: 0,
                    dst_alpha_blend_factor: 0,
                    alpha_blend_op: 0,
                    color_write_mask: 0,
                    _reserved_padding: 0,
                },
            },
            ColorBlendState::Enabled(att) => Self {
                kind: ColorBlendStateKindRepr::Enabled as u32,
                color_write_mask: 0,
                attachment: ColorBlendAttachmentRepr::from(att),
            },
        }
    }
}

impl From<DepthFormat> for DepthFormatRepr {
    fn from(value: DepthFormat) -> Self {
        match value {
            DepthFormat::D16Unorm => Self::D16Unorm,
            DepthFormat::D32Sfloat => Self::D32Sfloat,
            DepthFormat::D24UnormS8Uint => Self::D24UnormS8Uint,
        }
    }
}

impl From<GraphicsDynamicState> for GraphicsDynamicStateRepr {
    fn from(value: GraphicsDynamicState) -> Self {
        match value {
            GraphicsDynamicState::None => Self::None,
            GraphicsDynamicState::ViewportScissor => Self::ViewportScissor,
        }
    }
}

/// Keepalive backing for [`stage_graphics_kernel_descriptor`].
///
/// The staged [`GraphicsKernelDescriptorRepr`]'s pointer fields borrow
/// into these five Vecs (`Vec` moves don't reallocate, so the heap
/// buffer addresses stay stable when the Vecs are moved into this struct
/// after `as_ptr()` is taken). The caller MUST keep this struct alive for
/// the lifetime of the repr.
pub(crate) struct GraphicsKernelDescriptorReprStage {
    #[allow(dead_code)]
    pub stages_buf: Vec<GraphicsStageRepr>,
    #[allow(dead_code)]
    pub bindings_buf: Vec<GraphicsBindingSpecRepr>,
    #[allow(dead_code)]
    pub vertex_bindings_buf: Vec<VertexInputBindingRepr>,
    #[allow(dead_code)]
    pub vertex_attrs_buf: Vec<VertexInputAttributeRepr>,
    #[allow(dead_code)]
    pub color_formats_buf: Vec<u32>,
}

/// Stage a [`GraphicsKernelDescriptor`] to its `#[repr(C)]` mirror plus
/// the backing buffers the repr's pointer fields borrow into, ready for
/// the FullAccess `create_graphics_kernel` vtable call.
///
/// Returns `(repr, stage)`. The caller MUST keep `stage` alive for the
/// lifetime of `repr` (the repr's `stages_ptr` / `bindings_ptr` /
/// `vertex_input.bindings_ptr` / `vertex_input.attributes_ptr` /
/// `attachment_formats.color_ptr` all point into `stage`'s Vecs).
pub(crate) fn stage_graphics_kernel_descriptor(
    desc: &GraphicsKernelDescriptor<'_>,
) -> (
    GraphicsKernelDescriptorRepr,
    GraphicsKernelDescriptorReprStage,
) {
    let stages_buf: Vec<GraphicsStageRepr> = desc
        .stages
        .iter()
        .map(|s| GraphicsStageRepr {
            stage: GraphicsShaderStageRepr::from(s.stage) as u32,
            _reserved_padding: 0,
            spv_ptr: s.spv.as_ptr(),
            spv_len: s.spv.len(),
            entry_point_ptr: s.entry_point.as_ptr(),
            entry_point_len: s.entry_point.len(),
        })
        .collect();
    let bindings_buf: Vec<GraphicsBindingSpecRepr> = desc
        .bindings
        .iter()
        .map(GraphicsBindingSpecRepr::from)
        .collect();

    let (vertex_input_repr, vertex_bindings_buf, vertex_attrs_buf) =
        match &desc.pipeline_state.vertex_input {
            VertexInputState::None => (
                VertexInputStateRepr {
                    kind: VertexInputStateKindRepr::None as u32,
                    _reserved_padding: 0,
                    bindings_ptr: std::ptr::null(),
                    bindings_len: 0,
                    attributes_ptr: std::ptr::null(),
                    attributes_len: 0,
                },
                Vec::new(),
                Vec::new(),
            ),
            VertexInputState::Buffers {
                bindings,
                attributes,
            } => {
                let b: Vec<VertexInputBindingRepr> =
                    bindings.iter().map(VertexInputBindingRepr::from).collect();
                let a: Vec<VertexInputAttributeRepr> = attributes
                    .iter()
                    .map(VertexInputAttributeRepr::from)
                    .collect();
                let repr = VertexInputStateRepr {
                    kind: VertexInputStateKindRepr::Buffers as u32,
                    _reserved_padding: 0,
                    bindings_ptr: b.as_ptr(),
                    bindings_len: b.len(),
                    attributes_ptr: a.as_ptr(),
                    attributes_len: a.len(),
                };
                (repr, b, a)
            }
        };

    let color_formats_buf: Vec<u32> = desc
        .pipeline_state
        .attachment_formats
        .color
        .iter()
        .map(|f| *f as u32)
        .collect();
    let attachment_formats_repr = AttachmentFormatsRepr {
        color_ptr: color_formats_buf.as_ptr(),
        color_len: color_formats_buf.len(),
        has_depth: if desc.pipeline_state.attachment_formats.depth.is_some() {
            1
        } else {
            0
        },
        depth: desc
            .pipeline_state
            .attachment_formats
            .depth
            .map(|d| DepthFormatRepr::from(d) as u32)
            .unwrap_or(0),
    };

    let pipeline_state = GraphicsPipelineStateRepr {
        topology: PrimitiveTopologyRepr::from(desc.pipeline_state.topology) as u32,
        _reserved_padding1: 0,
        vertex_input: vertex_input_repr,
        rasterization: RasterizationStateRepr::from(desc.pipeline_state.rasterization),
        multisample: MultisampleStateRepr::from(desc.pipeline_state.multisample),
        depth_stencil: DepthStencilStateRepr::from(desc.pipeline_state.depth_stencil),
        color_blend: ColorBlendStateRepr::from(desc.pipeline_state.color_blend),
        attachment_formats: attachment_formats_repr,
        dynamic_state: GraphicsDynamicStateRepr::from(desc.pipeline_state.dynamic_state) as u32,
        _reserved_padding2: 0,
    };

    let repr = GraphicsKernelDescriptorRepr {
        label_ptr: desc.label.as_ptr(),
        label_len: desc.label.len(),
        stages_ptr: stages_buf.as_ptr(),
        stages_len: stages_buf.len(),
        bindings_ptr: bindings_buf.as_ptr(),
        bindings_len: bindings_buf.len(),
        push_constants: GraphicsPushConstantsRepr::from(desc.push_constants),
        pipeline_state,
        descriptor_sets_in_flight: desc.descriptor_sets_in_flight,
        _reserved_padding: 0,
    };

    let stage = GraphicsKernelDescriptorReprStage {
        stages_buf,
        bindings_buf,
        vertex_bindings_buf,
        vertex_attrs_buf,
        color_formats_buf,
    };
    (repr, stage)
}

#[cfg(test)]
mod staging_tests {
    use super::*;
    use streamlib_plugin_abi::{
        ColorBlendStateKindRepr, DepthStencilStateKindRepr, GraphicsShaderStageRepr,
        PrimitiveTopologyRepr, VertexInputStateKindRepr,
    };

    // Minimal non-empty SPIR-V-shaped byte blobs. Content is never
    // reflected here (that happens host-side); the staging fn only reads
    // the slice pointer + length.
    const VERT_SPV: &[u8] = &[0x03, 0x02, 0x23, 0x07, 0xAA, 0xBB];
    const FRAG_SPV: &[u8] = &[0x03, 0x02, 0x23, 0x07, 0xCC, 0xDD, 0xEE];

    #[test]
    fn fullscreen_effect_descriptor_stages_to_expected_repr() {
        let stages = [
            GraphicsStage::vertex(VERT_SPV),
            GraphicsStage::fragment(FRAG_SPV),
        ];
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let pipeline_state = GraphicsPipelineState {
            topology: PrimitiveTopology::TriangleList,
            vertex_input: VertexInputState::None,
            rasterization: RasterizationState::default(),
            multisample: MultisampleState::default(),
            depth_stencil: DepthStencilState::Disabled,
            color_blend: ColorBlendState::default(),
            attachment_formats: AttachmentFormats::color_only(TextureFormat::Rgba8Unorm),
            dynamic_state: GraphicsDynamicState::ViewportScissor,
        };
        let desc = GraphicsKernelDescriptor {
            label: "fullscreen_effect",
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants {
                size: 16,
                stages: GraphicsShaderStageFlags::FRAGMENT,
            },
            pipeline_state,
            descriptor_sets_in_flight: 2,
        };

        let (repr, stage) = stage_graphics_kernel_descriptor(&desc);

        // Top-level scalars + lengths.
        assert_eq!(repr.label_len, "fullscreen_effect".len());
        assert_eq!(repr.stages_len, 2);
        assert_eq!(repr.bindings_len, 1);
        assert_eq!(repr.descriptor_sets_in_flight, 2);
        assert_eq!(repr.push_constants.size, 16);
        assert_eq!(
            repr.push_constants.stages,
            GraphicsShaderStageFlags::FRAGMENT.bits()
        );

        // Pointer fields point into the keepalive Vecs (not dangling / null).
        assert_eq!(repr.stages_ptr, stage.stages_buf.as_ptr());
        assert_eq!(repr.bindings_ptr, stage.bindings_buf.as_ptr());

        // Stage discriminants + SPIR-V slice plumbing.
        assert_eq!(
            stage.stages_buf[0].stage,
            GraphicsShaderStageRepr::Vertex as u32
        );
        assert_eq!(stage.stages_buf[0].spv_len, VERT_SPV.len());
        assert_eq!(stage.stages_buf[0].spv_ptr, VERT_SPV.as_ptr());
        assert_eq!(stage.stages_buf[0].entry_point_len, "main".len());
        assert_eq!(
            stage.stages_buf[1].stage,
            GraphicsShaderStageRepr::Fragment as u32
        );
        assert_eq!(stage.stages_buf[1].spv_len, FRAG_SPV.len());

        // Binding mapping: sampled-texture kind + fragment stage mask.
        assert_eq!(
            stage.bindings_buf[0].kind,
            GraphicsBindingKindRepr::SampledTexture as u32
        );
        assert_eq!(stage.bindings_buf[0].binding, 0);
        assert_eq!(
            stage.bindings_buf[0].stages,
            GraphicsShaderStageFlags::FRAGMENT.bits()
        );

        // Pipeline-state scalars.
        assert_eq!(
            repr.pipeline_state.topology,
            PrimitiveTopologyRepr::TriangleList as u32
        );
        assert_eq!(
            repr.pipeline_state.dynamic_state,
            GraphicsDynamicStateRepr::ViewportScissor as u32
        );

        // VertexInputState::None → null slice pointers + Buffers kind absent.
        assert_eq!(
            repr.pipeline_state.vertex_input.kind,
            VertexInputStateKindRepr::None as u32
        );
        assert!(repr.pipeline_state.vertex_input.bindings_ptr.is_null());
        assert_eq!(repr.pipeline_state.vertex_input.bindings_len, 0);
        assert!(stage.vertex_bindings_buf.is_empty());
        assert!(stage.vertex_attrs_buf.is_empty());

        // DepthStencilState::Disabled.
        assert_eq!(
            repr.pipeline_state.depth_stencil.kind,
            DepthStencilStateKindRepr::Disabled as u32
        );

        // ColorBlendState::default() == Disabled { RGBA }.
        assert_eq!(
            repr.pipeline_state.color_blend.kind,
            ColorBlendStateKindRepr::Disabled as u32
        );
        assert_eq!(
            repr.pipeline_state.color_blend.color_write_mask,
            ColorWriteMask::RGBA.bits()
        );

        // AttachmentFormats: one color format, no depth.
        assert_eq!(repr.pipeline_state.attachment_formats.color_len, 1);
        assert_eq!(repr.pipeline_state.attachment_formats.has_depth, 0);
        assert_eq!(
            repr.pipeline_state.attachment_formats.color_ptr,
            stage.color_formats_buf.as_ptr()
        );
        assert_eq!(stage.color_formats_buf[0], TextureFormat::Rgba8Unorm as u32);
    }

    #[test]
    fn vertex_buffers_and_depth_stage_into_repr() {
        // Exercises the VertexInputState::Buffers + depth + enabled-blend
        // arms so the keepalive Vecs for vertex bindings/attributes carry
        // real data and the tagged-union discriminants map correctly.
        let stages = [
            GraphicsStage::vertex(VERT_SPV),
            GraphicsStage::fragment(FRAG_SPV),
        ];
        let bindings: [GraphicsBindingSpec; 0] = [];
        let vertex_input = VertexInputState::Buffers {
            bindings: vec![VertexInputBinding {
                binding: 0,
                stride: 32,
                input_rate: VertexInputRate::Vertex,
            }],
            attributes: vec![
                VertexInputAttribute {
                    location: 0,
                    binding: 0,
                    format: VertexAttributeFormat::Rgb32Float,
                    offset: 0,
                },
                VertexInputAttribute {
                    location: 1,
                    binding: 0,
                    format: VertexAttributeFormat::Rg32Float,
                    offset: 12,
                },
            ],
        };
        let pipeline_state = GraphicsPipelineState {
            topology: PrimitiveTopology::TriangleStrip,
            vertex_input,
            rasterization: RasterizationState {
                polygon_mode: PolygonMode::Fill,
                cull_mode: CullMode::Back,
                front_face: FrontFace::Clockwise,
                line_width: 2.0,
            },
            multisample: MultisampleState::default(),
            depth_stencil: DepthStencilState::Enabled {
                depth_test: DepthCompareOp::LessOrEqual,
                depth_write: true,
            },
            color_blend: ColorBlendState::Enabled(ColorBlendAttachment::ALPHA_OVER),
            attachment_formats: AttachmentFormats {
                color: vec![TextureFormat::Bgra8Unorm],
                depth: Some(DepthFormat::D32Sfloat),
            },
            dynamic_state: GraphicsDynamicState::ViewportScissor,
        };
        let desc = GraphicsKernelDescriptor {
            label: "mesh",
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants::NONE,
            pipeline_state,
            descriptor_sets_in_flight: 3,
        };

        let (repr, stage) = stage_graphics_kernel_descriptor(&desc);

        assert_eq!(repr.bindings_len, 0);
        assert_eq!(repr.descriptor_sets_in_flight, 3);
        assert_eq!(repr.push_constants.size, 0);

        // Buffers arm: two attributes, one binding, pointers into keepalive.
        assert_eq!(
            repr.pipeline_state.vertex_input.kind,
            VertexInputStateKindRepr::Buffers as u32
        );
        assert_eq!(repr.pipeline_state.vertex_input.bindings_len, 1);
        assert_eq!(repr.pipeline_state.vertex_input.attributes_len, 2);
        assert_eq!(
            repr.pipeline_state.vertex_input.bindings_ptr,
            stage.vertex_bindings_buf.as_ptr()
        );
        assert_eq!(
            repr.pipeline_state.vertex_input.attributes_ptr,
            stage.vertex_attrs_buf.as_ptr()
        );
        assert_eq!(stage.vertex_bindings_buf[0].stride, 32);
        assert_eq!(
            stage.vertex_attrs_buf[1].format,
            VertexAttributeFormatRepr::Rg32Float as u32
        );
        assert_eq!(stage.vertex_attrs_buf[1].offset, 12);

        // Rasterization scalars round-trip.
        assert_eq!(
            repr.pipeline_state.rasterization.cull_mode,
            CullModeRepr::Back as u32
        );
        assert_eq!(
            repr.pipeline_state.rasterization.front_face,
            FrontFaceRepr::Clockwise as u32
        );
        assert_eq!(repr.pipeline_state.rasterization.line_width, 2.0);

        // DepthStencilState::Enabled.
        assert_eq!(
            repr.pipeline_state.depth_stencil.kind,
            DepthStencilStateKindRepr::Enabled as u32
        );
        assert_eq!(
            repr.pipeline_state.depth_stencil.depth_test,
            DepthCompareOpRepr::LessOrEqual as u32
        );
        assert_eq!(repr.pipeline_state.depth_stencil.depth_write, 1);

        // ColorBlendState::Enabled(ALPHA_OVER).
        assert_eq!(
            repr.pipeline_state.color_blend.kind,
            ColorBlendStateKindRepr::Enabled as u32
        );
        assert_eq!(
            repr.pipeline_state
                .color_blend
                .attachment
                .src_color_blend_factor,
            BlendFactorRepr::SrcAlpha as u32
        );
        assert_eq!(
            repr.pipeline_state
                .color_blend
                .attachment
                .dst_color_blend_factor,
            BlendFactorRepr::OneMinusSrcAlpha as u32
        );

        // AttachmentFormats: one color + depth present.
        assert_eq!(repr.pipeline_state.attachment_formats.has_depth, 1);
        assert_eq!(
            repr.pipeline_state.attachment_formats.depth,
            DepthFormatRepr::D32Sfloat as u32
        );
    }
}
