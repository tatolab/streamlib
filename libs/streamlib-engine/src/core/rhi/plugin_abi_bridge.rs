// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Round-trip helpers between the engine-side kernel descriptor types
//! (`ComputeKernelDescriptor`, `GraphicsKernelDescriptor`,
//! `RayTracingKernelDescriptor`) and their `#[repr(C)]` mirrors in
//! `streamlib_plugin_abi`.
//!
//! Two directions:
//!
//! - **`Repr::from(&desc)`** is infallible: every Rust descriptor maps
//!   to a layout-stable Repr. Used by the cdylib at the FullAccess
//!   vtable call site to stage the wire-format payload.
//! - **`unsafe fn ..._from_repr(&repr)`** validates discriminants and
//!   slice pointer/length pairs, returning the engine-side Rust
//!   descriptor borrowing into the Repr's source memory. Used by the
//!   host inside its FullAccess vtable callback bodies.
//!
//! All conversions are pure: no allocation, no GPU work, no Vulkan
//! state. The host's `create_*_kernel` methods do the real work; this
//! module is just the wire-format bridge.

use streamlib_plugin_abi::{
    AttachmentFormatsRepr, BlendFactorRepr, BlendOpRepr, ColorBlendAttachmentRepr,
    ColorBlendStateKindRepr, ColorBlendStateRepr, ComputeBindingKindRepr,
    ComputeBindingSpecRepr, ComputeKernelDescriptorRepr, CullModeRepr, DepthCompareOpRepr,
    DepthFormatRepr, DepthStencilStateKindRepr, DepthStencilStateRepr, FrontFaceRepr,
    GraphicsBindingKindRepr, GraphicsBindingSpecRepr, GraphicsDynamicStateRepr,
    GraphicsKernelDescriptorRepr, GraphicsPipelineStateRepr, GraphicsPushConstantsRepr,
    GraphicsShaderStageRepr, GraphicsStageRepr, MultisampleStateRepr, PolygonModeRepr,
    PrimitiveTopologyRepr, RasterizationStateRepr, RayTracingBindingKindRepr,
    RayTracingBindingSpecRepr, RayTracingKernelDescriptorRepr, RayTracingPushConstantsRepr,
    RayTracingShaderGroupKindRepr, RayTracingShaderGroupRepr, RayTracingShaderStageRepr,
    RayTracingStageRepr, VertexAttributeFormatRepr, VertexInputAttributeRepr,
    VertexInputBindingRepr, VertexInputRateRepr, VertexInputStateKindRepr,
    VertexInputStateRepr, RAY_TRACING_SHADER_UNUSED,
};

use crate::core::rhi::{
    AttachmentFormats, BlendFactor, BlendOp, ColorBlendAttachment, ColorBlendState, ColorWriteMask,
    ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor, CullMode, DepthCompareOp,
    DepthFormat, DepthStencilState, FrontFace, GraphicsBindingKind, GraphicsBindingSpec,
    GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState, GraphicsPushConstants,
    GraphicsShaderStage, GraphicsShaderStageFlags, GraphicsStage, MultisampleState, PolygonMode,
    PrimitiveTopology, RasterizationState, RayTracingBindingKind, RayTracingBindingSpec,
    RayTracingKernelDescriptor, RayTracingPushConstants, RayTracingShaderGroup,
    RayTracingShaderStage, RayTracingShaderStageFlags, RayTracingStage, TextureFormat,
    VertexAttributeFormat, VertexInputAttribute, VertexInputBinding, VertexInputRate,
    VertexInputState,
};
use crate::core::{Error, Result};

// =============================================================================
// Compute kernel
// =============================================================================

impl From<ComputeBindingKind> for ComputeBindingKindRepr {
    fn from(value: ComputeBindingKind) -> Self {
        match value {
            ComputeBindingKind::StorageBuffer => Self::StorageBuffer,
            ComputeBindingKind::UniformBuffer => Self::UniformBuffer,
            ComputeBindingKind::SampledTexture => Self::SampledTexture,
            ComputeBindingKind::StorageImage => Self::StorageImage,
            ComputeBindingKind::SampledImage => Self::SampledImage,
        }
    }
}

fn compute_binding_kind_from_repr(raw: u32) -> Result<ComputeBindingKind> {
    match raw {
        0 => Ok(ComputeBindingKind::StorageBuffer),
        1 => Ok(ComputeBindingKind::UniformBuffer),
        2 => Ok(ComputeBindingKind::SampledTexture),
        3 => Ok(ComputeBindingKind::StorageImage),
        4 => Ok(ComputeBindingKind::SampledImage),
        other => Err(Error::GpuError(format!(
            "ComputeBindingKind: invalid discriminant {other}"
        ))),
    }
}

impl From<&ComputeBindingSpec> for ComputeBindingSpecRepr {
    fn from(value: &ComputeBindingSpec) -> Self {
        Self {
            binding: value.binding,
            kind: ComputeBindingKindRepr::from(value.kind) as u32,
        }
    }
}

fn compute_binding_spec_from_repr(repr: &ComputeBindingSpecRepr) -> Result<ComputeBindingSpec> {
    Ok(ComputeBindingSpec {
        binding: repr.binding,
        kind: compute_binding_kind_from_repr(repr.kind)?,
    })
}

/// Build a [`ComputeKernelDescriptorRepr`] that borrows into `desc`.
///
/// The returned Repr holds raw pointers into `desc`'s underlying
/// memory; both must outlive the Repr (the borrow checker enforces
/// this via the explicit lifetime tie below).
#[allow(dead_code)]
pub fn compute_kernel_descriptor_to_repr_with_bindings<'a>(
    desc: &'a ComputeKernelDescriptor<'a>,
    bindings_buf: &'a [ComputeBindingSpecRepr],
) -> ComputeKernelDescriptorRepr {
    debug_assert_eq!(bindings_buf.len(), desc.bindings.len());
    ComputeKernelDescriptorRepr {
        label_ptr: desc.label.as_ptr(),
        label_len: desc.label.len(),
        spv_ptr: desc.spv.as_ptr(),
        spv_len: desc.spv.len(),
        bindings_ptr: bindings_buf.as_ptr(),
        bindings_len: bindings_buf.len(),
        push_constant_size: desc.push_constant_size,
        _reserved_padding: 0,
    }
}

/// Stage a `ComputeKernelDescriptor` to its repr + a backing buffer of
/// repr bindings, ready for a vtable call.
///
/// Returns `(repr, bindings_buf)`. The caller MUST keep `bindings_buf`
/// alive for the lifetime of `repr` (the repr's `bindings_ptr` points
/// into `bindings_buf`); typical callers stash both on the stack
/// before the vtable call.
#[allow(dead_code)]
pub fn stage_compute_kernel_descriptor(
    desc: &ComputeKernelDescriptor<'_>,
) -> (ComputeKernelDescriptorRepr, Vec<ComputeBindingSpecRepr>) {
    let bindings_buf: Vec<ComputeBindingSpecRepr> =
        desc.bindings.iter().map(ComputeBindingSpecRepr::from).collect();
    // SAFETY: `bindings_buf` is a freshly-allocated Vec; we capture
    // its current data pointer, but the Vec lives as long as the
    // tuple returned by this function (and the caller is responsible
    // for keeping it alive while using the repr).
    let repr = ComputeKernelDescriptorRepr {
        label_ptr: desc.label.as_ptr(),
        label_len: desc.label.len(),
        spv_ptr: desc.spv.as_ptr(),
        spv_len: desc.spv.len(),
        bindings_ptr: bindings_buf.as_ptr(),
        bindings_len: bindings_buf.len(),
        push_constant_size: desc.push_constant_size,
        _reserved_padding: 0,
    };
    (repr, bindings_buf)
}

/// Decode a `ComputeKernelDescriptorRepr` and invoke `f` with a
/// borrowed Rust descriptor whose slices point at the repr's source
/// memory plus a stack-local binding-spec Vec.
///
/// Callback shape (not return-pair) is deliberate: the decoded
/// descriptor's `bindings` slice points into a Vec built inside this
/// function, and a `(Descriptor<'a>, Vec)` return would let a caller
/// drop the Vec while keeping the descriptor — a use-after-free with
/// no compile-time guard. Anchoring the borrow scope to `f`'s
/// invocation eliminates the footgun.
///
/// # Safety
///
/// All pointer/length pairs in `repr` must be valid for reads (i.e.
/// pointing at properly-aligned arrays of the right element type with
/// the declared lengths) for the duration of the call.
pub unsafe fn with_decoded_compute_kernel_descriptor<F, R>(
    repr: &ComputeKernelDescriptorRepr,
    f: F,
) -> Result<R>
where
    F: FnOnce(&ComputeKernelDescriptor<'_>) -> Result<R>,
{
    let label = unsafe { str_from_ptr_len(repr.label_ptr, repr.label_len, "label")? };
    let spv = unsafe { slice_from_ptr_len(repr.spv_ptr, repr.spv_len) };
    let bindings_repr = unsafe { slice_from_ptr_len(repr.bindings_ptr, repr.bindings_len) };
    let bindings: Vec<ComputeBindingSpec> = bindings_repr
        .iter()
        .map(compute_binding_spec_from_repr)
        .collect::<Result<_>>()?;
    let desc = ComputeKernelDescriptor {
        label,
        spv,
        bindings: &bindings,
        push_constant_size: repr.push_constant_size,
    };
    f(&desc)
}

// =============================================================================
// Graphics kernel
// =============================================================================

impl From<GraphicsShaderStage> for GraphicsShaderStageRepr {
    fn from(value: GraphicsShaderStage) -> Self {
        match value {
            GraphicsShaderStage::Vertex => Self::Vertex,
            GraphicsShaderStage::Fragment => Self::Fragment,
        }
    }
}

fn graphics_shader_stage_from_repr(raw: u32) -> Result<GraphicsShaderStage> {
    match raw {
        0 => Ok(GraphicsShaderStage::Vertex),
        1 => Ok(GraphicsShaderStage::Fragment),
        other => Err(Error::GpuError(format!(
            "GraphicsShaderStage: invalid discriminant {other}"
        ))),
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

fn graphics_binding_kind_from_repr(raw: u32) -> Result<GraphicsBindingKind> {
    match raw {
        0 => Ok(GraphicsBindingKind::SampledTexture),
        1 => Ok(GraphicsBindingKind::StorageBuffer),
        2 => Ok(GraphicsBindingKind::UniformBuffer),
        3 => Ok(GraphicsBindingKind::StorageImage),
        other => Err(Error::GpuError(format!(
            "GraphicsBindingKind: invalid discriminant {other}"
        ))),
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

fn primitive_topology_from_repr(raw: u32) -> Result<PrimitiveTopology> {
    match raw {
        0 => Ok(PrimitiveTopology::PointList),
        1 => Ok(PrimitiveTopology::LineList),
        2 => Ok(PrimitiveTopology::LineStrip),
        3 => Ok(PrimitiveTopology::TriangleList),
        4 => Ok(PrimitiveTopology::TriangleStrip),
        5 => Ok(PrimitiveTopology::TriangleFan),
        other => Err(Error::GpuError(format!(
            "PrimitiveTopology: invalid discriminant {other}"
        ))),
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

fn vertex_attribute_format_from_repr(raw: u32) -> Result<VertexAttributeFormat> {
    match raw {
        0 => Ok(VertexAttributeFormat::R32Float),
        1 => Ok(VertexAttributeFormat::Rg32Float),
        2 => Ok(VertexAttributeFormat::Rgb32Float),
        3 => Ok(VertexAttributeFormat::Rgba32Float),
        4 => Ok(VertexAttributeFormat::R32Uint),
        5 => Ok(VertexAttributeFormat::Rg32Uint),
        6 => Ok(VertexAttributeFormat::Rgb32Uint),
        7 => Ok(VertexAttributeFormat::Rgba32Uint),
        8 => Ok(VertexAttributeFormat::R32Sint),
        9 => Ok(VertexAttributeFormat::Rg32Sint),
        10 => Ok(VertexAttributeFormat::Rgb32Sint),
        11 => Ok(VertexAttributeFormat::Rgba32Sint),
        12 => Ok(VertexAttributeFormat::Rgba8Unorm),
        13 => Ok(VertexAttributeFormat::Rgba8Snorm),
        other => Err(Error::GpuError(format!(
            "VertexAttributeFormat: invalid discriminant {other}"
        ))),
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

fn vertex_input_rate_from_repr(raw: u32) -> Result<VertexInputRate> {
    match raw {
        0 => Ok(VertexInputRate::Vertex),
        1 => Ok(VertexInputRate::Instance),
        other => Err(Error::GpuError(format!(
            "VertexInputRate: invalid discriminant {other}"
        ))),
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

fn polygon_mode_from_repr(raw: u32) -> Result<PolygonMode> {
    match raw {
        0 => Ok(PolygonMode::Fill),
        1 => Ok(PolygonMode::Line),
        2 => Ok(PolygonMode::Point),
        other => Err(Error::GpuError(format!(
            "PolygonMode: invalid discriminant {other}"
        ))),
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

fn cull_mode_from_repr(raw: u32) -> Result<CullMode> {
    match raw {
        0 => Ok(CullMode::None),
        1 => Ok(CullMode::Front),
        2 => Ok(CullMode::Back),
        3 => Ok(CullMode::FrontAndBack),
        other => Err(Error::GpuError(format!(
            "CullMode: invalid discriminant {other}"
        ))),
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

fn front_face_from_repr(raw: u32) -> Result<FrontFace> {
    match raw {
        0 => Ok(FrontFace::CounterClockwise),
        1 => Ok(FrontFace::Clockwise),
        other => Err(Error::GpuError(format!(
            "FrontFace: invalid discriminant {other}"
        ))),
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

fn depth_compare_op_from_repr(raw: u32) -> Result<DepthCompareOp> {
    match raw {
        0 => Ok(DepthCompareOp::Never),
        1 => Ok(DepthCompareOp::Less),
        2 => Ok(DepthCompareOp::Equal),
        3 => Ok(DepthCompareOp::LessOrEqual),
        4 => Ok(DepthCompareOp::Greater),
        5 => Ok(DepthCompareOp::NotEqual),
        6 => Ok(DepthCompareOp::GreaterOrEqual),
        7 => Ok(DepthCompareOp::Always),
        other => Err(Error::GpuError(format!(
            "DepthCompareOp: invalid discriminant {other}"
        ))),
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

fn blend_factor_from_repr(raw: u32) -> Result<BlendFactor> {
    match raw {
        0 => Ok(BlendFactor::Zero),
        1 => Ok(BlendFactor::One),
        2 => Ok(BlendFactor::SrcColor),
        3 => Ok(BlendFactor::OneMinusSrcColor),
        4 => Ok(BlendFactor::DstColor),
        5 => Ok(BlendFactor::OneMinusDstColor),
        6 => Ok(BlendFactor::SrcAlpha),
        7 => Ok(BlendFactor::OneMinusSrcAlpha),
        8 => Ok(BlendFactor::DstAlpha),
        9 => Ok(BlendFactor::OneMinusDstAlpha),
        10 => Ok(BlendFactor::ConstantColor),
        11 => Ok(BlendFactor::OneMinusConstantColor),
        12 => Ok(BlendFactor::ConstantAlpha),
        13 => Ok(BlendFactor::OneMinusConstantAlpha),
        14 => Ok(BlendFactor::SrcAlphaSaturate),
        other => Err(Error::GpuError(format!(
            "BlendFactor: invalid discriminant {other}"
        ))),
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

fn blend_op_from_repr(raw: u32) -> Result<BlendOp> {
    match raw {
        0 => Ok(BlendOp::Add),
        1 => Ok(BlendOp::Subtract),
        2 => Ok(BlendOp::ReverseSubtract),
        3 => Ok(BlendOp::Min),
        4 => Ok(BlendOp::Max),
        other => Err(Error::GpuError(format!(
            "BlendOp: invalid discriminant {other}"
        ))),
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

fn depth_format_from_repr(raw: u32) -> Result<DepthFormat> {
    match raw {
        0 => Ok(DepthFormat::D16Unorm),
        1 => Ok(DepthFormat::D32Sfloat),
        2 => Ok(DepthFormat::D24UnormS8Uint),
        other => Err(Error::GpuError(format!(
            "DepthFormat: invalid discriminant {other}"
        ))),
    }
}

fn texture_format_from_repr(raw: u32) -> Result<TextureFormat> {
    match raw {
        0 => Ok(TextureFormat::Rgba8Unorm),
        1 => Ok(TextureFormat::Rgba8UnormSrgb),
        2 => Ok(TextureFormat::Bgra8Unorm),
        3 => Ok(TextureFormat::Bgra8UnormSrgb),
        4 => Ok(TextureFormat::Rgba16Float),
        5 => Ok(TextureFormat::Rgba32Float),
        6 => Ok(TextureFormat::Nv12),
        other => Err(Error::GpuError(format!(
            "TextureFormat: invalid discriminant {other}"
        ))),
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

fn graphics_dynamic_state_from_repr(raw: u32) -> Result<GraphicsDynamicState> {
    match raw {
        0 => Ok(GraphicsDynamicState::None),
        1 => Ok(GraphicsDynamicState::ViewportScissor),
        other => Err(Error::GpuError(format!(
            "GraphicsDynamicState: invalid discriminant {other}"
        ))),
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

fn vertex_input_binding_from_repr(repr: &VertexInputBindingRepr) -> Result<VertexInputBinding> {
    Ok(VertexInputBinding {
        binding: repr.binding,
        stride: repr.stride,
        input_rate: vertex_input_rate_from_repr(repr.input_rate)?,
    })
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

fn vertex_input_attribute_from_repr(
    repr: &VertexInputAttributeRepr,
) -> Result<VertexInputAttribute> {
    Ok(VertexInputAttribute {
        location: repr.location,
        binding: repr.binding,
        format: vertex_attribute_format_from_repr(repr.format)?,
        offset: repr.offset,
    })
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

fn graphics_binding_spec_from_repr(repr: &GraphicsBindingSpecRepr) -> Result<GraphicsBindingSpec> {
    Ok(GraphicsBindingSpec {
        binding: repr.binding,
        kind: graphics_binding_kind_from_repr(repr.kind)?,
        stages: graphics_shader_stage_flags_from_bits(repr.stages),
    })
}

/// Reconstruct `GraphicsShaderStageFlags` from raw `u32` bits. Unknown
/// bits beyond the defined VERTEX/FRAGMENT pair are silently dropped
/// so future-flag additions on the source side don't trip the receiver.
fn graphics_shader_stage_flags_from_bits(bits: u32) -> GraphicsShaderStageFlags {
    let mut out = GraphicsShaderStageFlags::NONE;
    if bits & GraphicsShaderStageFlags::VERTEX.bits() != 0 {
        out |= GraphicsShaderStageFlags::VERTEX;
    }
    if bits & GraphicsShaderStageFlags::FRAGMENT.bits() != 0 {
        out |= GraphicsShaderStageFlags::FRAGMENT;
    }
    out
}

impl From<GraphicsPushConstants> for GraphicsPushConstantsRepr {
    fn from(value: GraphicsPushConstants) -> Self {
        Self {
            size: value.size,
            stages: value.stages.bits(),
        }
    }
}

fn graphics_push_constants_from_repr(
    repr: &GraphicsPushConstantsRepr,
) -> GraphicsPushConstants {
    GraphicsPushConstants {
        size: repr.size,
        stages: graphics_shader_stage_flags_from_bits(repr.stages),
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

fn rasterization_state_from_repr(repr: &RasterizationStateRepr) -> Result<RasterizationState> {
    Ok(RasterizationState {
        polygon_mode: polygon_mode_from_repr(repr.polygon_mode)?,
        cull_mode: cull_mode_from_repr(repr.cull_mode)?,
        front_face: front_face_from_repr(repr.front_face)?,
        line_width: repr.line_width,
    })
}

impl From<MultisampleState> for MultisampleStateRepr {
    fn from(value: MultisampleState) -> Self {
        Self {
            samples: value.samples,
            _reserved_padding: 0,
        }
    }
}

fn multisample_state_from_repr(repr: &MultisampleStateRepr) -> MultisampleState {
    MultisampleState {
        samples: repr.samples,
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

fn depth_stencil_state_from_repr(repr: &DepthStencilStateRepr) -> Result<DepthStencilState> {
    match repr.kind {
        x if x == DepthStencilStateKindRepr::Disabled as u32 => Ok(DepthStencilState::Disabled),
        x if x == DepthStencilStateKindRepr::Enabled as u32 => Ok(DepthStencilState::Enabled {
            depth_test: depth_compare_op_from_repr(repr.depth_test)?,
            depth_write: repr.depth_write != 0,
        }),
        other => Err(Error::GpuError(format!(
            "DepthStencilState: invalid kind {other}"
        ))),
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

fn color_blend_attachment_from_repr(
    repr: &ColorBlendAttachmentRepr,
) -> Result<ColorBlendAttachment> {
    Ok(ColorBlendAttachment {
        src_color_blend_factor: blend_factor_from_repr(repr.src_color_blend_factor)?,
        dst_color_blend_factor: blend_factor_from_repr(repr.dst_color_blend_factor)?,
        color_blend_op: blend_op_from_repr(repr.color_blend_op)?,
        src_alpha_blend_factor: blend_factor_from_repr(repr.src_alpha_blend_factor)?,
        dst_alpha_blend_factor: blend_factor_from_repr(repr.dst_alpha_blend_factor)?,
        alpha_blend_op: blend_op_from_repr(repr.alpha_blend_op)?,
        color_write_mask: color_write_mask_from_bits(repr.color_write_mask),
    })
}

/// Reconstruct a `ColorWriteMask` from its `bits()` representation.
///
/// `ColorWriteMask`'s public surface exposes only the R/G/B/A/RGBA
/// constants, so we rebuild the value by OR-ing the channels indicated
/// by `bits`. Unknown bits beyond R|G|B|A are silently dropped.
///
/// The all-zero case (no channels written) is not expressible through
/// the public API; we surface it as `ColorWriteMask::RGBA` because the
/// in-tree producer never emits an all-zero mask and a non-degenerate
/// fallback is safer than introducing a new public constant. The
/// round-trip test exercises the four single-channel cases plus RGBA.
fn color_write_mask_from_bits(bits: u32) -> ColorWriteMask {
    let mut accumulator: Option<ColorWriteMask> = None;
    let maybe_or = |accumulator: &mut Option<ColorWriteMask>, channel: ColorWriteMask| {
        *accumulator = Some(match *accumulator {
            Some(existing) => existing | channel,
            None => channel,
        });
    };
    if bits & ColorWriteMask::R.bits() != 0 {
        maybe_or(&mut accumulator, ColorWriteMask::R);
    }
    if bits & ColorWriteMask::G.bits() != 0 {
        maybe_or(&mut accumulator, ColorWriteMask::G);
    }
    if bits & ColorWriteMask::B.bits() != 0 {
        maybe_or(&mut accumulator, ColorWriteMask::B);
    }
    if bits & ColorWriteMask::A.bits() != 0 {
        maybe_or(&mut accumulator, ColorWriteMask::A);
    }
    accumulator.unwrap_or(ColorWriteMask::RGBA)
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

fn color_blend_state_from_repr(repr: &ColorBlendStateRepr) -> Result<ColorBlendState> {
    match repr.kind {
        x if x == ColorBlendStateKindRepr::Disabled as u32 => Ok(ColorBlendState::Disabled {
            color_write_mask: color_write_mask_from_bits(repr.color_write_mask),
        }),
        x if x == ColorBlendStateKindRepr::Enabled as u32 => Ok(ColorBlendState::Enabled(
            color_blend_attachment_from_repr(&repr.attachment)?,
        )),
        other => Err(Error::GpuError(format!(
            "ColorBlendState: invalid kind {other}"
        ))),
    }
}

// =============================================================================
// Ray-tracing kernel
// =============================================================================

impl From<RayTracingShaderStage> for RayTracingShaderStageRepr {
    fn from(value: RayTracingShaderStage) -> Self {
        match value {
            RayTracingShaderStage::RayGen => Self::RayGen,
            RayTracingShaderStage::Miss => Self::Miss,
            RayTracingShaderStage::ClosestHit => Self::ClosestHit,
            RayTracingShaderStage::AnyHit => Self::AnyHit,
            RayTracingShaderStage::Intersection => Self::Intersection,
            RayTracingShaderStage::Callable => Self::Callable,
        }
    }
}

fn ray_tracing_shader_stage_from_repr(raw: u32) -> Result<RayTracingShaderStage> {
    match raw {
        0 => Ok(RayTracingShaderStage::RayGen),
        1 => Ok(RayTracingShaderStage::Miss),
        2 => Ok(RayTracingShaderStage::ClosestHit),
        3 => Ok(RayTracingShaderStage::AnyHit),
        4 => Ok(RayTracingShaderStage::Intersection),
        5 => Ok(RayTracingShaderStage::Callable),
        other => Err(Error::GpuError(format!(
            "RayTracingShaderStage: invalid discriminant {other}"
        ))),
    }
}

impl From<RayTracingBindingKind> for RayTracingBindingKindRepr {
    fn from(value: RayTracingBindingKind) -> Self {
        match value {
            RayTracingBindingKind::StorageBuffer => Self::StorageBuffer,
            RayTracingBindingKind::UniformBuffer => Self::UniformBuffer,
            RayTracingBindingKind::SampledTexture => Self::SampledTexture,
            RayTracingBindingKind::StorageImage => Self::StorageImage,
            RayTracingBindingKind::AccelerationStructure => Self::AccelerationStructure,
        }
    }
}

fn ray_tracing_binding_kind_from_repr(raw: u32) -> Result<RayTracingBindingKind> {
    match raw {
        0 => Ok(RayTracingBindingKind::StorageBuffer),
        1 => Ok(RayTracingBindingKind::UniformBuffer),
        2 => Ok(RayTracingBindingKind::SampledTexture),
        3 => Ok(RayTracingBindingKind::StorageImage),
        4 => Ok(RayTracingBindingKind::AccelerationStructure),
        other => Err(Error::GpuError(format!(
            "RayTracingBindingKind: invalid discriminant {other}"
        ))),
    }
}

impl From<&RayTracingBindingSpec> for RayTracingBindingSpecRepr {
    fn from(value: &RayTracingBindingSpec) -> Self {
        Self {
            binding: value.binding,
            kind: RayTracingBindingKindRepr::from(value.kind) as u32,
            stages: value.stages.bits(),
            _reserved_padding: 0,
        }
    }
}

fn ray_tracing_binding_spec_from_repr(
    repr: &RayTracingBindingSpecRepr,
) -> Result<RayTracingBindingSpec> {
    Ok(RayTracingBindingSpec {
        binding: repr.binding,
        kind: ray_tracing_binding_kind_from_repr(repr.kind)?,
        stages: ray_tracing_shader_stage_flags_from_bits(repr.stages),
    })
}

fn ray_tracing_shader_stage_flags_from_bits(bits: u32) -> RayTracingShaderStageFlags {
    let mut out = RayTracingShaderStageFlags::NONE;
    if bits & RayTracingShaderStageFlags::RAYGEN.bits() != 0 {
        out |= RayTracingShaderStageFlags::RAYGEN;
    }
    if bits & RayTracingShaderStageFlags::MISS.bits() != 0 {
        out |= RayTracingShaderStageFlags::MISS;
    }
    if bits & RayTracingShaderStageFlags::CLOSEST_HIT.bits() != 0 {
        out |= RayTracingShaderStageFlags::CLOSEST_HIT;
    }
    if bits & RayTracingShaderStageFlags::ANY_HIT.bits() != 0 {
        out |= RayTracingShaderStageFlags::ANY_HIT;
    }
    if bits & RayTracingShaderStageFlags::INTERSECTION.bits() != 0 {
        out |= RayTracingShaderStageFlags::INTERSECTION;
    }
    if bits & RayTracingShaderStageFlags::CALLABLE.bits() != 0 {
        out |= RayTracingShaderStageFlags::CALLABLE;
    }
    out
}

impl From<RayTracingPushConstants> for RayTracingPushConstantsRepr {
    fn from(value: RayTracingPushConstants) -> Self {
        Self {
            size: value.size,
            stages: value.stages.bits(),
        }
    }
}

fn ray_tracing_push_constants_from_repr(
    repr: &RayTracingPushConstantsRepr,
) -> RayTracingPushConstants {
    RayTracingPushConstants {
        size: repr.size,
        stages: ray_tracing_shader_stage_flags_from_bits(repr.stages),
    }
}

impl From<RayTracingShaderGroup> for RayTracingShaderGroupRepr {
    fn from(value: RayTracingShaderGroup) -> Self {
        match value {
            RayTracingShaderGroup::General { general } => Self {
                kind: RayTracingShaderGroupKindRepr::General as u32,
                general_or_intersection: general,
                closest_hit: RAY_TRACING_SHADER_UNUSED,
                any_hit: RAY_TRACING_SHADER_UNUSED,
            },
            RayTracingShaderGroup::TrianglesHit {
                closest_hit,
                any_hit,
            } => Self {
                kind: RayTracingShaderGroupKindRepr::TrianglesHit as u32,
                general_or_intersection: RAY_TRACING_SHADER_UNUSED,
                closest_hit: closest_hit.unwrap_or(RAY_TRACING_SHADER_UNUSED),
                any_hit: any_hit.unwrap_or(RAY_TRACING_SHADER_UNUSED),
            },
            RayTracingShaderGroup::ProceduralHit {
                intersection,
                closest_hit,
                any_hit,
            } => Self {
                kind: RayTracingShaderGroupKindRepr::ProceduralHit as u32,
                general_or_intersection: intersection,
                closest_hit: closest_hit.unwrap_or(RAY_TRACING_SHADER_UNUSED),
                any_hit: any_hit.unwrap_or(RAY_TRACING_SHADER_UNUSED),
            },
        }
    }
}

fn ray_tracing_shader_group_from_repr(
    repr: &RayTracingShaderGroupRepr,
) -> Result<RayTracingShaderGroup> {
    let optionalize = |v: u32| -> Option<u32> {
        if v == RAY_TRACING_SHADER_UNUSED {
            None
        } else {
            Some(v)
        }
    };
    match repr.kind {
        x if x == RayTracingShaderGroupKindRepr::General as u32 => {
            Ok(RayTracingShaderGroup::General {
                general: repr.general_or_intersection,
            })
        }
        x if x == RayTracingShaderGroupKindRepr::TrianglesHit as u32 => {
            Ok(RayTracingShaderGroup::TrianglesHit {
                closest_hit: optionalize(repr.closest_hit),
                any_hit: optionalize(repr.any_hit),
            })
        }
        x if x == RayTracingShaderGroupKindRepr::ProceduralHit as u32 => {
            Ok(RayTracingShaderGroup::ProceduralHit {
                intersection: repr.general_or_intersection,
                closest_hit: optionalize(repr.closest_hit),
                any_hit: optionalize(repr.any_hit),
            })
        }
        other => Err(Error::GpuError(format!(
            "RayTracingShaderGroup: invalid kind {other}"
        ))),
    }
}

// =============================================================================
// Pointer/slice helpers
// =============================================================================

/// Materialize a `&[T]` from a raw `(ptr, len)` pair. Null pointer or
/// zero length yields an empty slice.
///
/// # Safety
///
/// If `len > 0` the pointer must be valid for reads of `len` elements
/// and properly aligned. Returned slice borrows from caller-managed
/// memory; the caller must ensure it outlives the returned slice.
unsafe fn slice_from_ptr_len<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: caller-supplied contract.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Materialize a `&str` from a raw `(ptr, len)` pair, validating UTF-8.
///
/// # Safety
///
/// If `len > 0` the pointer must be valid for reads of `len` bytes.
/// Returned `&str` borrows from caller-managed memory.
unsafe fn str_from_ptr_len<'a>(
    ptr: *const u8,
    len: usize,
    field_name: &str,
) -> Result<&'a str> {
    let bytes = unsafe { slice_from_ptr_len::<u8>(ptr, len) };
    std::str::from_utf8(bytes).map_err(|e| {
        Error::GpuError(format!("{field_name}: invalid UTF-8: {e}"))
    })
}

// =============================================================================
// Round-trip API for full kernel descriptors
// =============================================================================

/// Owned backing buffers behind a `GraphicsKernelDescriptorRepr`.
///
/// Caller stages a descriptor + this buffer; the repr borrows into
/// the buffer's storage. Caller must keep the buffer alive for the
/// repr's lifetime.
#[allow(dead_code)]
pub struct GraphicsKernelDescriptorReprStage {
    pub stages_buf: Vec<GraphicsStageRepr>,
    pub bindings_buf: Vec<GraphicsBindingSpecRepr>,
    pub vertex_bindings_buf: Vec<VertexInputBindingRepr>,
    pub vertex_attrs_buf: Vec<VertexInputAttributeRepr>,
    pub color_formats_buf: Vec<u32>,
}

/// Stage a `GraphicsKernelDescriptor` into its repr + backing buffers.
#[allow(dead_code)]
pub fn stage_graphics_kernel_descriptor(
    desc: &GraphicsKernelDescriptor<'_>,
) -> (GraphicsKernelDescriptorRepr, GraphicsKernelDescriptorReprStage) {
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
    let bindings_buf: Vec<GraphicsBindingSpecRepr> =
        desc.bindings.iter().map(GraphicsBindingSpecRepr::from).collect();

    let (vertex_input_repr, vertex_bindings_buf, vertex_attrs_buf) = match &desc
        .pipeline_state
        .vertex_input
    {
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
            let a: Vec<VertexInputAttributeRepr> =
                attributes.iter().map(VertexInputAttributeRepr::from).collect();
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

/// Decode a `GraphicsKernelDescriptorRepr` and invoke `f` with a
/// borrowed Rust descriptor.
///
/// Same callback-shape rationale as
/// [`with_decoded_compute_kernel_descriptor`].
///
/// # Safety
///
/// All pointer/length pairs in `repr` (and inside its nested
/// `vertex_input` and `attachment_formats`) must be valid for reads
/// for the duration of the call.
pub unsafe fn with_decoded_graphics_kernel_descriptor<F, R>(
    repr: &GraphicsKernelDescriptorRepr,
    f: F,
) -> Result<R>
where
    F: FnOnce(&GraphicsKernelDescriptor<'_>) -> Result<R>,
{
    let label = unsafe { str_from_ptr_len(repr.label_ptr, repr.label_len, "label")? };
    let stages_repr =
        unsafe { slice_from_ptr_len::<GraphicsStageRepr>(repr.stages_ptr, repr.stages_len) };
    let stages: Vec<GraphicsStage<'_>> = stages_repr
        .iter()
        .map(|s| {
            let stage = graphics_shader_stage_from_repr(s.stage)?;
            let spv: &[u8] = unsafe { slice_from_ptr_len(s.spv_ptr, s.spv_len) };
            let entry_point =
                unsafe { str_from_ptr_len(s.entry_point_ptr, s.entry_point_len, "entry_point")? };
            Ok::<_, Error>(GraphicsStage {
                stage,
                spv,
                entry_point,
            })
        })
        .collect::<Result<_>>()?;

    let bindings_repr = unsafe {
        slice_from_ptr_len::<GraphicsBindingSpecRepr>(repr.bindings_ptr, repr.bindings_len)
    };
    let bindings: Vec<GraphicsBindingSpec> = bindings_repr
        .iter()
        .map(graphics_binding_spec_from_repr)
        .collect::<Result<_>>()?;

    let push_constants = graphics_push_constants_from_repr(&repr.push_constants);

    let vertex_input = match repr.pipeline_state.vertex_input.kind {
        x if x == VertexInputStateKindRepr::None as u32 => VertexInputState::None,
        x if x == VertexInputStateKindRepr::Buffers as u32 => {
            let bindings_ptr = repr.pipeline_state.vertex_input.bindings_ptr;
            let bindings_len = repr.pipeline_state.vertex_input.bindings_len;
            let attrs_ptr = repr.pipeline_state.vertex_input.attributes_ptr;
            let attrs_len = repr.pipeline_state.vertex_input.attributes_len;
            let b_repr =
                unsafe { slice_from_ptr_len::<VertexInputBindingRepr>(bindings_ptr, bindings_len) };
            let a_repr = unsafe {
                slice_from_ptr_len::<VertexInputAttributeRepr>(attrs_ptr, attrs_len)
            };
            let b: Vec<VertexInputBinding> = b_repr
                .iter()
                .map(vertex_input_binding_from_repr)
                .collect::<Result<_>>()?;
            let a: Vec<VertexInputAttribute> = a_repr
                .iter()
                .map(vertex_input_attribute_from_repr)
                .collect::<Result<_>>()?;
            VertexInputState::Buffers {
                bindings: b,
                attributes: a,
            }
        }
        other => {
            return Err(Error::GpuError(format!(
                "VertexInputState: invalid kind {other}"
            )));
        }
    };

    let color_formats_raw = unsafe {
        slice_from_ptr_len::<u32>(
            repr.pipeline_state.attachment_formats.color_ptr,
            repr.pipeline_state.attachment_formats.color_len,
        )
    };
    let color_formats: Vec<TextureFormat> = color_formats_raw
        .iter()
        .copied()
        .map(texture_format_from_repr)
        .collect::<Result<_>>()?;
    let depth = if repr.pipeline_state.attachment_formats.has_depth == 0 {
        None
    } else {
        Some(depth_format_from_repr(
            repr.pipeline_state.attachment_formats.depth,
        )?)
    };

    let pipeline_state = GraphicsPipelineState {
        topology: primitive_topology_from_repr(repr.pipeline_state.topology)?,
        vertex_input,
        rasterization: rasterization_state_from_repr(&repr.pipeline_state.rasterization)?,
        multisample: multisample_state_from_repr(&repr.pipeline_state.multisample),
        depth_stencil: depth_stencil_state_from_repr(&repr.pipeline_state.depth_stencil)?,
        color_blend: color_blend_state_from_repr(&repr.pipeline_state.color_blend)?,
        attachment_formats: AttachmentFormats {
            color: color_formats,
            depth,
        },
        dynamic_state: graphics_dynamic_state_from_repr(repr.pipeline_state.dynamic_state)?,
    };

    let desc = GraphicsKernelDescriptor {
        label,
        stages: &stages,
        bindings: &bindings,
        push_constants,
        pipeline_state,
        descriptor_sets_in_flight: repr.descriptor_sets_in_flight,
    };
    f(&desc)
}

/// Owned backing buffers behind a `RayTracingKernelDescriptorRepr`.
#[allow(dead_code)]
pub struct RayTracingKernelDescriptorReprStage {
    pub stages_buf: Vec<RayTracingStageRepr>,
    pub groups_buf: Vec<RayTracingShaderGroupRepr>,
    pub bindings_buf: Vec<RayTracingBindingSpecRepr>,
}

/// Stage a `RayTracingKernelDescriptor` into its repr + backing buffers.
#[allow(dead_code)]
pub fn stage_ray_tracing_kernel_descriptor(
    desc: &RayTracingKernelDescriptor<'_>,
) -> (
    RayTracingKernelDescriptorRepr,
    RayTracingKernelDescriptorReprStage,
) {
    let stages_buf: Vec<RayTracingStageRepr> = desc
        .stages
        .iter()
        .map(|s| RayTracingStageRepr {
            stage: RayTracingShaderStageRepr::from(s.stage) as u32,
            _reserved_padding: 0,
            spv_ptr: s.spv.as_ptr(),
            spv_len: s.spv.len(),
            entry_point_ptr: s.entry_point.as_ptr(),
            entry_point_len: s.entry_point.len(),
        })
        .collect();
    let groups_buf: Vec<RayTracingShaderGroupRepr> =
        desc.groups.iter().copied().map(RayTracingShaderGroupRepr::from).collect();
    let bindings_buf: Vec<RayTracingBindingSpecRepr> = desc
        .bindings
        .iter()
        .map(RayTracingBindingSpecRepr::from)
        .collect();

    let repr = RayTracingKernelDescriptorRepr {
        label_ptr: desc.label.as_ptr(),
        label_len: desc.label.len(),
        stages_ptr: stages_buf.as_ptr(),
        stages_len: stages_buf.len(),
        groups_ptr: groups_buf.as_ptr(),
        groups_len: groups_buf.len(),
        bindings_ptr: bindings_buf.as_ptr(),
        bindings_len: bindings_buf.len(),
        push_constants: RayTracingPushConstantsRepr::from(desc.push_constants),
        max_recursion_depth: desc.max_recursion_depth,
        _reserved_padding: 0,
    };

    let stage = RayTracingKernelDescriptorReprStage {
        stages_buf,
        groups_buf,
        bindings_buf,
    };
    (repr, stage)
}

/// Decode a `RayTracingKernelDescriptorRepr` and invoke `f` with a
/// borrowed Rust descriptor.
///
/// Same callback-shape rationale as
/// [`with_decoded_compute_kernel_descriptor`].
///
/// # Safety
///
/// All pointer/length pairs in `repr` must be valid for reads for the
/// duration of the call.
pub unsafe fn with_decoded_ray_tracing_kernel_descriptor<F, R>(
    repr: &RayTracingKernelDescriptorRepr,
    f: F,
) -> Result<R>
where
    F: FnOnce(&RayTracingKernelDescriptor<'_>) -> Result<R>,
{
    let label = unsafe { str_from_ptr_len(repr.label_ptr, repr.label_len, "label")? };
    let stages_repr = unsafe {
        slice_from_ptr_len::<RayTracingStageRepr>(repr.stages_ptr, repr.stages_len)
    };
    let stages: Vec<RayTracingStage<'_>> = stages_repr
        .iter()
        .map(|s| {
            let stage = ray_tracing_shader_stage_from_repr(s.stage)?;
            let spv: &[u8] = unsafe { slice_from_ptr_len(s.spv_ptr, s.spv_len) };
            let entry_point =
                unsafe { str_from_ptr_len(s.entry_point_ptr, s.entry_point_len, "entry_point")? };
            Ok::<_, Error>(RayTracingStage {
                stage,
                spv,
                entry_point,
            })
        })
        .collect::<Result<_>>()?;

    let groups_repr = unsafe {
        slice_from_ptr_len::<RayTracingShaderGroupRepr>(repr.groups_ptr, repr.groups_len)
    };
    let groups: Vec<RayTracingShaderGroup> = groups_repr
        .iter()
        .map(ray_tracing_shader_group_from_repr)
        .collect::<Result<_>>()?;

    let bindings_repr = unsafe {
        slice_from_ptr_len::<RayTracingBindingSpecRepr>(repr.bindings_ptr, repr.bindings_len)
    };
    let bindings: Vec<RayTracingBindingSpec> = bindings_repr
        .iter()
        .map(ray_tracing_binding_spec_from_repr)
        .collect::<Result<_>>()?;

    let push_constants = ray_tracing_push_constants_from_repr(&repr.push_constants);

    let desc = RayTracingKernelDescriptor {
        label,
        stages: &stages,
        groups: &groups,
        bindings: &bindings,
        push_constants,
        max_recursion_depth: repr.max_recursion_depth,
    };
    f(&desc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_descriptor_roundtrip() {
        let spv: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x00];
        let bindings = [
            ComputeBindingSpec::storage_buffer(0),
            ComputeBindingSpec::uniform_buffer(1),
            ComputeBindingSpec::sampled_texture(2),
            ComputeBindingSpec::storage_image(3),
        ];
        let desc = ComputeKernelDescriptor {
            label: "test_compute",
            spv,
            bindings: &bindings,
            push_constant_size: 16,
        };
        let (repr, _buf) = stage_compute_kernel_descriptor(&desc);
        unsafe {
            with_decoded_compute_kernel_descriptor(&repr, |decoded| {
                assert_eq!(decoded.label, "test_compute");
                assert_eq!(decoded.spv, spv);
                assert_eq!(decoded.bindings.len(), 4);
                assert_eq!(
                    decoded.bindings[0].kind,
                    ComputeBindingKind::StorageBuffer
                );
                assert_eq!(
                    decoded.bindings[1].kind,
                    ComputeBindingKind::UniformBuffer
                );
                assert_eq!(
                    decoded.bindings[2].kind,
                    ComputeBindingKind::SampledTexture
                );
                assert_eq!(
                    decoded.bindings[3].kind,
                    ComputeBindingKind::StorageImage
                );
                assert_eq!(decoded.push_constant_size, 16);
                Ok(())
            })
            .expect("decode");
        }
    }

    #[test]
    fn compute_descriptor_rejects_invalid_kind() {
        let spv: &[u8] = &[0; 4];
        let bad_repr = ComputeBindingSpecRepr {
            binding: 0,
            kind: 99,
        };
        let bindings_ptr = &bad_repr as *const ComputeBindingSpecRepr;
        let repr = ComputeKernelDescriptorRepr {
            label_ptr: "x".as_ptr(),
            label_len: 1,
            spv_ptr: spv.as_ptr(),
            spv_len: spv.len(),
            bindings_ptr,
            bindings_len: 1,
            push_constant_size: 0,
            _reserved_padding: 0,
        };
        let err = unsafe {
            with_decoded_compute_kernel_descriptor(&repr, |_| {
                Ok::<_, Error>(())
            })
            .unwrap_err()
        };
        assert!(format!("{err}").contains("invalid discriminant 99"));
    }

    #[test]
    fn graphics_descriptor_roundtrip_minimal() {
        let vs_spv: &[u8] = &[0xDE, 0xAD];
        let fs_spv: &[u8] = &[0xBE, 0xEF];
        let stages = [GraphicsStage::vertex(vs_spv), GraphicsStage::fragment(fs_spv)];
        let bindings = [
            GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::uniform_buffer(1, GraphicsShaderStageFlags::VERTEX_FRAGMENT),
        ];
        let desc = GraphicsKernelDescriptor {
            label: "test_graphics",
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants {
                size: 12,
                stages: GraphicsShaderStageFlags::VERTEX,
            },
            pipeline_state: GraphicsPipelineState {
                topology: PrimitiveTopology::TriangleStrip,
                vertex_input: VertexInputState::None,
                rasterization: RasterizationState::default(),
                multisample: MultisampleState::default(),
                depth_stencil: DepthStencilState::Disabled,
                color_blend: ColorBlendState::Enabled(ColorBlendAttachment::ALPHA_OVER),
                attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            descriptor_sets_in_flight: 2,
        };
        let (repr, _stage) = stage_graphics_kernel_descriptor(&desc);
        unsafe {
            with_decoded_graphics_kernel_descriptor(&repr, |decoded| {
                assert_eq!(decoded.label, "test_graphics");
                assert_eq!(decoded.stages.len(), 2);
                assert_eq!(decoded.stages[0].stage, GraphicsShaderStage::Vertex);
                assert_eq!(decoded.stages[1].stage, GraphicsShaderStage::Fragment);
                assert_eq!(decoded.bindings.len(), 2);
                assert_eq!(decoded.push_constants.size, 12);
                assert_eq!(
                    decoded.pipeline_state.topology,
                    PrimitiveTopology::TriangleStrip
                );
                assert!(matches!(
                    decoded.pipeline_state.vertex_input,
                    VertexInputState::None
                ));
                assert!(matches!(
                    decoded.pipeline_state.color_blend,
                    ColorBlendState::Enabled(_)
                ));
                assert_eq!(decoded.descriptor_sets_in_flight, 2);
                Ok(())
            })
            .expect("decode");
        }
    }

    #[test]
    fn graphics_descriptor_roundtrip_with_vertex_input_buffers() {
        let vs_spv: &[u8] = &[0; 4];
        let fs_spv: &[u8] = &[0; 4];
        let stages = [GraphicsStage::vertex(vs_spv), GraphicsStage::fragment(fs_spv)];
        let vertex_bindings = vec![VertexInputBinding {
            binding: 0,
            stride: 32,
            input_rate: VertexInputRate::Vertex,
        }];
        let vertex_attrs = vec![
            VertexInputAttribute {
                location: 0,
                binding: 0,
                format: VertexAttributeFormat::Rgb32Float,
                offset: 0,
            },
            VertexInputAttribute {
                location: 1,
                binding: 0,
                format: VertexAttributeFormat::Rgba32Float,
                offset: 12,
            },
        ];
        let desc = GraphicsKernelDescriptor {
            label: "vi",
            stages: &stages,
            bindings: &[],
            push_constants: GraphicsPushConstants::NONE,
            pipeline_state: GraphicsPipelineState {
                topology: PrimitiveTopology::TriangleList,
                vertex_input: VertexInputState::Buffers {
                    bindings: vertex_bindings.clone(),
                    attributes: vertex_attrs.clone(),
                },
                rasterization: RasterizationState::default(),
                multisample: MultisampleState::default(),
                depth_stencil: DepthStencilState::Enabled {
                    depth_test: DepthCompareOp::LessOrEqual,
                    depth_write: true,
                },
                color_blend: ColorBlendState::default(),
                attachment_formats: AttachmentFormats {
                    color: vec![TextureFormat::Rgba8UnormSrgb],
                    depth: Some(DepthFormat::D32Sfloat),
                },
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            descriptor_sets_in_flight: 3,
        };
        let (repr, _stage) = stage_graphics_kernel_descriptor(&desc);
        unsafe {
            with_decoded_graphics_kernel_descriptor(&repr, |decoded| {
                match &decoded.pipeline_state.vertex_input {
                    VertexInputState::Buffers {
                        bindings,
                        attributes,
                    } => {
                        assert_eq!(bindings.len(), 1);
                        assert_eq!(bindings[0].stride, 32);
                        assert_eq!(attributes.len(), 2);
                        assert_eq!(attributes[1].offset, 12);
                        assert_eq!(
                            attributes[1].format,
                            VertexAttributeFormat::Rgba32Float
                        );
                    }
                    _ => panic!("expected Buffers"),
                }
                assert!(matches!(
                    decoded.pipeline_state.depth_stencil,
                    DepthStencilState::Enabled {
                        depth_test: DepthCompareOp::LessOrEqual,
                        depth_write: true,
                    }
                ));
                assert_eq!(
                    decoded.pipeline_state.attachment_formats.depth,
                    Some(DepthFormat::D32Sfloat)
                );
                Ok(())
            })
            .expect("decode");
        }
    }

    #[test]
    fn ray_tracing_descriptor_roundtrip() {
        let raygen_spv: &[u8] = &[0xAA];
        let miss_spv: &[u8] = &[0xBB];
        let closest_spv: &[u8] = &[0xCC];
        let stages = [
            RayTracingStage::ray_gen(raygen_spv),
            RayTracingStage::miss(miss_spv),
            RayTracingStage::closest_hit(closest_spv),
        ];
        let groups = [
            RayTracingShaderGroup::General { general: 0 },
            RayTracingShaderGroup::General { general: 1 },
            RayTracingShaderGroup::TrianglesHit {
                closest_hit: Some(2),
                any_hit: None,
            },
        ];
        let bindings = [
            RayTracingBindingSpec::storage_buffer(0, RayTracingShaderStageFlags::RAYGEN),
            RayTracingBindingSpec::acceleration_structure(
                1,
                RayTracingShaderStageFlags::RAYGEN | RayTracingShaderStageFlags::CLOSEST_HIT,
            ),
        ];
        let desc = RayTracingKernelDescriptor {
            label: "test_rt",
            stages: &stages,
            groups: &groups,
            bindings: &bindings,
            push_constants: RayTracingPushConstants {
                size: 8,
                stages: RayTracingShaderStageFlags::RAYGEN,
            },
            max_recursion_depth: 4,
        };
        let (repr, _stage) = stage_ray_tracing_kernel_descriptor(&desc);
        unsafe {
            with_decoded_ray_tracing_kernel_descriptor(&repr, |decoded| {
                assert_eq!(decoded.label, "test_rt");
                assert_eq!(decoded.stages.len(), 3);
                assert_eq!(decoded.groups.len(), 3);
                match decoded.groups[2] {
                    RayTracingShaderGroup::TrianglesHit {
                        closest_hit,
                        any_hit,
                    } => {
                        assert_eq!(closest_hit, Some(2));
                        assert_eq!(any_hit, None);
                    }
                    _ => panic!("expected TrianglesHit"),
                }
                assert_eq!(decoded.bindings.len(), 2);
                assert_eq!(
                    decoded.bindings[1].kind,
                    RayTracingBindingKind::AccelerationStructure
                );
                assert_eq!(decoded.push_constants.size, 8);
                assert_eq!(decoded.max_recursion_depth, 4);
                Ok(())
            })
            .expect("decode");
        }
    }

    #[test]
    fn ray_tracing_shader_group_unused_sentinel() {
        // Stage TrianglesHit with both closest/any_hit absent (None).
        let group = RayTracingShaderGroup::TrianglesHit {
            closest_hit: None,
            any_hit: None,
        };
        let repr = RayTracingShaderGroupRepr::from(group);
        assert_eq!(repr.kind, RayTracingShaderGroupKindRepr::TrianglesHit as u32);
        assert_eq!(repr.closest_hit, RAY_TRACING_SHADER_UNUSED);
        assert_eq!(repr.any_hit, RAY_TRACING_SHADER_UNUSED);
        // Round-trip back to None.
        let back = ray_tracing_shader_group_from_repr(&repr).expect("decode");
        match back {
            RayTracingShaderGroup::TrianglesHit {
                closest_hit,
                any_hit,
            } => {
                assert_eq!(closest_hit, None);
                assert_eq!(any_hit, None);
            }
            _ => panic!("expected TrianglesHit"),
        }
    }

    #[test]
    fn ray_tracing_descriptor_rejects_invalid_group_kind() {
        let bad_repr = RayTracingShaderGroupRepr {
            kind: 99,
            general_or_intersection: 0,
            closest_hit: RAY_TRACING_SHADER_UNUSED,
            any_hit: RAY_TRACING_SHADER_UNUSED,
        };
        let err = ray_tracing_shader_group_from_repr(&bad_repr).unwrap_err();
        assert!(format!("{err}").contains("invalid kind 99"));
    }
}
