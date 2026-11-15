// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::vk;

use crate::core::rhi::{
    BlendFactor, BlendOp, ColorWriteMask, CullMode, DepthCompareOp, DepthFormat, FrontFace,
    GraphicsBindingKind, GraphicsShaderStage, GraphicsShaderStageFlags, IndexType, PolygonMode,
    PrimitiveTopology, TextureFormat, VertexAttributeFormat, VertexInputRate,
};

pub(super) fn descriptor_kind_to_vk(kind: GraphicsBindingKind) -> vk::DescriptorType {
    match kind {
        GraphicsBindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        GraphicsBindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        GraphicsBindingKind::SampledTexture => vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        GraphicsBindingKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
    }
}

pub(super) fn shader_stage_flags_to_vk(stages: GraphicsShaderStageFlags) -> vk::ShaderStageFlags {
    let mut out = vk::ShaderStageFlags::empty();
    if stages.contains(GraphicsShaderStageFlags::VERTEX) {
        out |= vk::ShaderStageFlags::VERTEX;
    }
    if stages.contains(GraphicsShaderStageFlags::FRAGMENT) {
        out |= vk::ShaderStageFlags::FRAGMENT;
    }
    out
}

pub(super) fn graphics_stage_to_vk(stage: GraphicsShaderStage) -> vk::ShaderStageFlags {
    match stage {
        GraphicsShaderStage::Vertex => vk::ShaderStageFlags::VERTEX,
        GraphicsShaderStage::Fragment => vk::ShaderStageFlags::FRAGMENT,
    }
}

pub(super) fn primitive_topology_to_vk(t: PrimitiveTopology) -> vk::PrimitiveTopology {
    match t {
        PrimitiveTopology::PointList => vk::PrimitiveTopology::POINT_LIST,
        PrimitiveTopology::LineList => vk::PrimitiveTopology::LINE_LIST,
        PrimitiveTopology::LineStrip => vk::PrimitiveTopology::LINE_STRIP,
        PrimitiveTopology::TriangleList => vk::PrimitiveTopology::TRIANGLE_LIST,
        PrimitiveTopology::TriangleStrip => vk::PrimitiveTopology::TRIANGLE_STRIP,
        PrimitiveTopology::TriangleFan => vk::PrimitiveTopology::TRIANGLE_FAN,
    }
}

pub(super) fn polygon_mode_to_vk(m: PolygonMode) -> vk::PolygonMode {
    match m {
        PolygonMode::Fill => vk::PolygonMode::FILL,
        PolygonMode::Line => vk::PolygonMode::LINE,
        PolygonMode::Point => vk::PolygonMode::POINT,
    }
}

pub(super) fn cull_mode_to_vk(m: CullMode) -> vk::CullModeFlags {
    match m {
        CullMode::None => vk::CullModeFlags::NONE,
        CullMode::Front => vk::CullModeFlags::FRONT,
        CullMode::Back => vk::CullModeFlags::BACK,
        CullMode::FrontAndBack => vk::CullModeFlags::FRONT_AND_BACK,
    }
}

pub(super) fn front_face_to_vk(f: FrontFace) -> vk::FrontFace {
    match f {
        FrontFace::CounterClockwise => vk::FrontFace::COUNTER_CLOCKWISE,
        FrontFace::Clockwise => vk::FrontFace::CLOCKWISE,
    }
}

pub(super) fn depth_compare_op_to_vk(c: DepthCompareOp) -> vk::CompareOp {
    match c {
        DepthCompareOp::Never => vk::CompareOp::NEVER,
        DepthCompareOp::Less => vk::CompareOp::LESS,
        DepthCompareOp::Equal => vk::CompareOp::EQUAL,
        DepthCompareOp::LessOrEqual => vk::CompareOp::LESS_OR_EQUAL,
        DepthCompareOp::Greater => vk::CompareOp::GREATER,
        DepthCompareOp::NotEqual => vk::CompareOp::NOT_EQUAL,
        DepthCompareOp::GreaterOrEqual => vk::CompareOp::GREATER_OR_EQUAL,
        DepthCompareOp::Always => vk::CompareOp::ALWAYS,
    }
}

pub(super) fn blend_factor_to_vk(b: BlendFactor) -> vk::BlendFactor {
    match b {
        BlendFactor::Zero => vk::BlendFactor::ZERO,
        BlendFactor::One => vk::BlendFactor::ONE,
        BlendFactor::SrcColor => vk::BlendFactor::SRC_COLOR,
        BlendFactor::OneMinusSrcColor => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        BlendFactor::DstColor => vk::BlendFactor::DST_COLOR,
        BlendFactor::OneMinusDstColor => vk::BlendFactor::ONE_MINUS_DST_COLOR,
        BlendFactor::SrcAlpha => vk::BlendFactor::SRC_ALPHA,
        BlendFactor::OneMinusSrcAlpha => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        BlendFactor::DstAlpha => vk::BlendFactor::DST_ALPHA,
        BlendFactor::OneMinusDstAlpha => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
        BlendFactor::ConstantColor => vk::BlendFactor::CONSTANT_COLOR,
        BlendFactor::OneMinusConstantColor => vk::BlendFactor::ONE_MINUS_CONSTANT_COLOR,
        BlendFactor::ConstantAlpha => vk::BlendFactor::CONSTANT_ALPHA,
        BlendFactor::OneMinusConstantAlpha => vk::BlendFactor::ONE_MINUS_CONSTANT_ALPHA,
        BlendFactor::SrcAlphaSaturate => vk::BlendFactor::SRC_ALPHA_SATURATE,
    }
}

pub(super) fn blend_op_to_vk(op: BlendOp) -> vk::BlendOp {
    match op {
        BlendOp::Add => vk::BlendOp::ADD,
        BlendOp::Subtract => vk::BlendOp::SUBTRACT,
        BlendOp::ReverseSubtract => vk::BlendOp::REVERSE_SUBTRACT,
        BlendOp::Min => vk::BlendOp::MIN,
        BlendOp::Max => vk::BlendOp::MAX,
    }
}

pub(super) fn color_write_mask_to_vk(m: ColorWriteMask) -> vk::ColorComponentFlags {
    let mut out = vk::ColorComponentFlags::empty();
    if (m.bits() & ColorWriteMask::R.bits()) != 0 {
        out |= vk::ColorComponentFlags::R;
    }
    if (m.bits() & ColorWriteMask::G.bits()) != 0 {
        out |= vk::ColorComponentFlags::G;
    }
    if (m.bits() & ColorWriteMask::B.bits()) != 0 {
        out |= vk::ColorComponentFlags::B;
    }
    if (m.bits() & ColorWriteMask::A.bits()) != 0 {
        out |= vk::ColorComponentFlags::A;
    }
    out
}

pub(super) fn vertex_attribute_format_to_vk(f: VertexAttributeFormat) -> vk::Format {
    match f {
        VertexAttributeFormat::R32Float => vk::Format::R32_SFLOAT,
        VertexAttributeFormat::Rg32Float => vk::Format::R32G32_SFLOAT,
        VertexAttributeFormat::Rgb32Float => vk::Format::R32G32B32_SFLOAT,
        VertexAttributeFormat::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        VertexAttributeFormat::R32Uint => vk::Format::R32_UINT,
        VertexAttributeFormat::Rg32Uint => vk::Format::R32G32_UINT,
        VertexAttributeFormat::Rgb32Uint => vk::Format::R32G32B32_UINT,
        VertexAttributeFormat::Rgba32Uint => vk::Format::R32G32B32A32_UINT,
        VertexAttributeFormat::R32Sint => vk::Format::R32_SINT,
        VertexAttributeFormat::Rg32Sint => vk::Format::R32G32_SINT,
        VertexAttributeFormat::Rgb32Sint => vk::Format::R32G32B32_SINT,
        VertexAttributeFormat::Rgba32Sint => vk::Format::R32G32B32A32_SINT,
        VertexAttributeFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        VertexAttributeFormat::Rgba8Snorm => vk::Format::R8G8B8A8_SNORM,
    }
}

pub(super) fn vertex_input_rate_to_vk(r: VertexInputRate) -> vk::VertexInputRate {
    match r {
        VertexInputRate::Vertex => vk::VertexInputRate::VERTEX,
        VertexInputRate::Instance => vk::VertexInputRate::INSTANCE,
    }
}

pub(super) fn texture_format_to_vk(f: TextureFormat) -> vk::Format {
    match f {
        TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        TextureFormat::Rgba8UnormSrgb => vk::Format::R8G8B8A8_SRGB,
        TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        TextureFormat::Bgra8UnormSrgb => vk::Format::B8G8R8A8_SRGB,
        TextureFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        TextureFormat::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        // NV12 isn't a valid color attachment; we leave the mapping for
        // sampling consumers and refuse it as an attachment format below.
        TextureFormat::Nv12 => vk::Format::G8_B8R8_2PLANE_420_UNORM,
    }
}

pub(super) fn depth_format_to_vk(f: DepthFormat) -> vk::Format {
    match f {
        DepthFormat::D16Unorm => vk::Format::D16_UNORM,
        DepthFormat::D32Sfloat => vk::Format::D32_SFLOAT,
        DepthFormat::D24UnormS8Uint => vk::Format::D24_UNORM_S8_UINT,
    }
}

pub(super) fn index_type_to_vk(t: IndexType) -> vk::IndexType {
    match t {
        IndexType::Uint16 => vk::IndexType::UINT16,
        IndexType::Uint32 => vk::IndexType::UINT32,
    }
}
