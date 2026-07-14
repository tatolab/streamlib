// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graphics-pipeline `#[repr(C)]` descriptor mirrors plus the draw/viewport/scissor
//! helpers used by [`crate::VulkanGraphicsKernelMethodsVTable`] dispatch.

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::GraphicsShaderStage`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsShaderStageRepr {
    Vertex = 0,
    Fragment = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsStage`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsStageRepr {
    /// `GraphicsShaderStageRepr` discriminant.
    pub stage: u32,
    pub _reserved_padding: u32,
    pub spv_ptr: *const u8,
    pub spv_len: usize,
    pub entry_point_ptr: *const u8,
    pub entry_point_len: usize,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::GraphicsBindingKind`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsBindingKindRepr {
    SampledTexture = 0,
    StorageBuffer = 1,
    UniformBuffer = 2,
    StorageImage = 3,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsBindingSpec`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsBindingSpecRepr {
    pub binding: u32,
    /// `GraphicsBindingKindRepr` discriminant.
    pub kind: u32,
    /// `GraphicsShaderStageFlags::bits()` — already a u32 bitflag set,
    /// crosses as `u32` directly.
    pub stages: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsPushConstants`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsPushConstantsRepr {
    pub size: u32,
    /// `GraphicsShaderStageFlags::bits()`.
    pub stages: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::PrimitiveTopology`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveTopologyRepr {
    PointList = 0,
    LineList = 1,
    LineStrip = 2,
    TriangleList = 3,
    TriangleStrip = 4,
    TriangleFan = 5,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::VertexAttributeFormat`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAttributeFormatRepr {
    R32Float = 0,
    Rg32Float = 1,
    Rgb32Float = 2,
    Rgba32Float = 3,
    R32Uint = 4,
    Rg32Uint = 5,
    Rgb32Uint = 6,
    Rgba32Uint = 7,
    R32Sint = 8,
    Rg32Sint = 9,
    Rgb32Sint = 10,
    Rgba32Sint = 11,
    Rgba8Unorm = 12,
    Rgba8Snorm = 13,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::VertexInputRate`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexInputRateRepr {
    Vertex = 0,
    Instance = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::VertexInputBinding`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VertexInputBindingRepr {
    pub binding: u32,
    pub stride: u32,
    /// `VertexInputRateRepr` discriminant.
    pub input_rate: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::VertexInputAttribute`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VertexInputAttributeRepr {
    pub location: u32,
    pub binding: u32,
    /// `VertexAttributeFormatRepr` discriminant.
    pub format: u32,
    pub offset: u32,
}

/// Discriminant for the [`VertexInputStateRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexInputStateKindRepr {
    None = 0,
    Buffers = 1,
}

/// Tagged-union mirror of `streamlib::core::rhi::VertexInputState`.
///
/// When `kind == None`, the slice pointers are null and lengths are 0.
/// When `kind == Buffers`, the slices borrow into caller-owned arrays.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VertexInputStateRepr {
    /// `VertexInputStateKindRepr` discriminant.
    pub kind: u32,
    pub _reserved_padding: u32,
    pub bindings_ptr: *const VertexInputBindingRepr,
    pub bindings_len: usize,
    pub attributes_ptr: *const VertexInputAttributeRepr,
    pub attributes_len: usize,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::PolygonMode`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonModeRepr {
    Fill = 0,
    Line = 1,
    Point = 2,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::CullMode`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CullModeRepr {
    None = 0,
    Front = 1,
    Back = 2,
    FrontAndBack = 3,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::FrontFace`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontFaceRepr {
    CounterClockwise = 0,
    Clockwise = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RasterizationState`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RasterizationStateRepr {
    /// `PolygonModeRepr` discriminant.
    pub polygon_mode: u32,
    /// `CullModeRepr` discriminant.
    pub cull_mode: u32,
    /// `FrontFaceRepr` discriminant.
    pub front_face: u32,
    pub line_width: f32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::MultisampleState`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MultisampleStateRepr {
    pub samples: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::DepthCompareOp`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthCompareOpRepr {
    Never = 0,
    Less = 1,
    Equal = 2,
    LessOrEqual = 3,
    Greater = 4,
    NotEqual = 5,
    GreaterOrEqual = 6,
    Always = 7,
}

/// Discriminant for the [`DepthStencilStateRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthStencilStateKindRepr {
    Disabled = 0,
    Enabled = 1,
}

/// Tagged-union mirror of `streamlib::core::rhi::DepthStencilState`.
///
/// `depth_test` and `depth_write` are ignored when `kind == Disabled`
/// (writer sets them to 0 / 0 by convention).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DepthStencilStateRepr {
    /// `DepthStencilStateKindRepr` discriminant.
    pub kind: u32,
    /// `DepthCompareOpRepr` discriminant. Ignored when `kind == Disabled`.
    pub depth_test: u32,
    /// Boolean as `u32` (0 = false, 1 = true). Ignored when `kind == Disabled`.
    pub depth_write: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::BlendFactor`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendFactorRepr {
    Zero = 0,
    One = 1,
    SrcColor = 2,
    OneMinusSrcColor = 3,
    DstColor = 4,
    OneMinusDstColor = 5,
    SrcAlpha = 6,
    OneMinusSrcAlpha = 7,
    DstAlpha = 8,
    OneMinusDstAlpha = 9,
    ConstantColor = 10,
    OneMinusConstantColor = 11,
    ConstantAlpha = 12,
    OneMinusConstantAlpha = 13,
    SrcAlphaSaturate = 14,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::BlendOp`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendOpRepr {
    Add = 0,
    Subtract = 1,
    ReverseSubtract = 2,
    Min = 3,
    Max = 4,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ColorBlendAttachment`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ColorBlendAttachmentRepr {
    /// `BlendFactorRepr` discriminant.
    pub src_color_blend_factor: u32,
    /// `BlendFactorRepr` discriminant.
    pub dst_color_blend_factor: u32,
    /// `BlendOpRepr` discriminant.
    pub color_blend_op: u32,
    /// `BlendFactorRepr` discriminant.
    pub src_alpha_blend_factor: u32,
    /// `BlendFactorRepr` discriminant.
    pub dst_alpha_blend_factor: u32,
    /// `BlendOpRepr` discriminant.
    pub alpha_blend_op: u32,
    /// `ColorWriteMask::bits()`.
    pub color_write_mask: u32,
    pub _reserved_padding: u32,
}

/// Discriminant for the [`ColorBlendStateRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorBlendStateKindRepr {
    Disabled = 0,
    Enabled = 1,
}

/// Tagged-union mirror of `streamlib::core::rhi::ColorBlendState`.
///
/// When `kind == Disabled`, `color_write_mask` carries the disabled
/// state's mask and `attachment` fields are zero (ignored on the host
/// side). When `kind == Enabled`, `color_write_mask` is zero and
/// `attachment` carries the full attachment description.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ColorBlendStateRepr {
    /// `ColorBlendStateKindRepr` discriminant.
    pub kind: u32,
    /// `ColorWriteMask::bits()` for the Disabled case; ignored when
    /// `kind == Enabled`.
    pub color_write_mask: u32,
    pub attachment: ColorBlendAttachmentRepr,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::DepthFormat`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthFormatRepr {
    D16Unorm = 0,
    D32Sfloat = 1,
    D24UnormS8Uint = 2,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::AttachmentFormats`.
///
/// `color` is a slice of `TextureFormat::repr(u32)` discriminants
/// (from `streamlib_consumer_rhi::TextureFormat`'s `#[repr(u32)]`).
/// `has_depth` is a boolean encoded as `u32`; when `has_depth == 0`,
/// `depth` is ignored.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AttachmentFormatsRepr {
    /// Pointer to an array of `streamlib_consumer_rhi::TextureFormat`
    /// discriminants.
    pub color_ptr: *const u32,
    pub color_len: usize,
    /// 0 = `None`, 1 = `Some(depth)`.
    pub has_depth: u32,
    /// `DepthFormatRepr` discriminant. Ignored when `has_depth == 0`.
    pub depth: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::GraphicsDynamicState`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsDynamicStateRepr {
    None = 0,
    ViewportScissor = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsPipelineState`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsPipelineStateRepr {
    /// `PrimitiveTopologyRepr` discriminant.
    pub topology: u32,
    pub _reserved_padding1: u32,
    pub vertex_input: VertexInputStateRepr,
    pub rasterization: RasterizationStateRepr,
    pub multisample: MultisampleStateRepr,
    pub depth_stencil: DepthStencilStateRepr,
    pub color_blend: ColorBlendStateRepr,
    pub attachment_formats: AttachmentFormatsRepr,
    /// `GraphicsDynamicStateRepr` discriminant.
    pub dynamic_state: u32,
    pub _reserved_padding2: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsKernelDescriptor`.
///
/// All pointer fields borrow into caller-owned memory and must
/// remain valid for the duration of the vtable call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsKernelDescriptorRepr {
    pub label_ptr: *const u8,
    pub label_len: usize,
    pub stages_ptr: *const GraphicsStageRepr,
    pub stages_len: usize,
    pub bindings_ptr: *const GraphicsBindingSpecRepr,
    pub bindings_len: usize,
    pub push_constants: GraphicsPushConstantsRepr,
    pub pipeline_state: GraphicsPipelineStateRepr,
    pub descriptor_sets_in_flight: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::IndexType`.
///
/// Discriminant carried on [`crate::DrawIndexedCallRepr`]'s sibling index-buffer
/// binding slot ([`crate::VulkanGraphicsKernelMethodsVTable::set_index_buffer`]).
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IndexTypeRepr {
    Uint16 = 0,
    Uint32 = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::Viewport`.
///
/// Field order matches the host-side struct; layout-locked by the
/// regression test in `layout_tests`.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ViewportRepr {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ScissorRect`.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ScissorRectRepr {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::DrawCall`.
///
/// `Option<Viewport>` / `Option<ScissorRect>` are encoded as
/// `<field>_present: u32` discriminator + a zero-initialized payload
/// when absent — `Option<T>` has no stable `#[repr(C)]` shape.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct DrawCallRepr {
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
    /// `1` when [`Self::viewport`] carries a value; `0` for `None`.
    pub viewport_present: u32,
    /// `1` when [`Self::scissor`] carries a value; `0` for `None`.
    pub scissor_present: u32,
    pub viewport: ViewportRepr,
    pub scissor: ScissorRectRepr,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::DrawIndexedCall`.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct DrawIndexedCallRepr {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub vertex_offset: i32,
    pub first_instance: u32,
    /// `1` when [`Self::viewport`] carries a value; `0` for `None`.
    pub viewport_present: u32,
    /// `1` when [`Self::scissor`] carries a value; `0` for `None`.
    pub scissor_present: u32,
    pub _reserved_padding: u32,
    pub viewport: ViewportRepr,
    pub scissor: ScissorRectRepr,
}

/// Discriminant for the [`crate::OffscreenDrawRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OffscreenDrawKindRepr {
    Draw = 0,
    DrawIndexed = 1,
}

/// `#[repr(C)]` tagged-union mirror of
/// `streamlib::vulkan::rhi::OffscreenDraw`.
///
/// Only the slot whose tag matches [`Self::kind`] is read:
/// - `Draw` → [`Self::draw_call`]
/// - `DrawIndexed` → [`Self::draw_indexed_call`]
///
/// The unused slot is zero-initialized; the host wrapper only
/// inspects the kind-matched payload.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct OffscreenDrawRepr {
    /// [`crate::OffscreenDrawKindRepr`] discriminant.
    pub kind: u32,
    pub _reserved_padding: u32,
    pub draw_call: DrawCallRepr,
    pub draw_indexed_call: DrawIndexedCallRepr,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn graphics_stage_repr_layout() {
        // u32 + u32 + 2 (ptr,len) pairs = 8 + 32 = 40 bytes.
        assert_eq!(size_of::<GraphicsStageRepr>(), 40);
        assert_eq!(align_of::<GraphicsStageRepr>(), 8);
        assert_eq!(offset_of!(GraphicsStageRepr, stage), 0);
        assert_eq!(offset_of!(GraphicsStageRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(GraphicsStageRepr, spv_ptr), 8);
        assert_eq!(offset_of!(GraphicsStageRepr, spv_len), 16);
        assert_eq!(offset_of!(GraphicsStageRepr, entry_point_ptr), 24);
        assert_eq!(offset_of!(GraphicsStageRepr, entry_point_len), 32);
    }

    #[test]
    fn graphics_binding_spec_repr_layout() {
        assert_eq!(size_of::<GraphicsBindingSpecRepr>(), 16);
        assert_eq!(align_of::<GraphicsBindingSpecRepr>(), 4);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, binding), 0);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, kind), 4);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, stages), 8);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, _reserved_padding), 12);
    }

    #[test]
    fn graphics_push_constants_repr_layout() {
        assert_eq!(size_of::<GraphicsPushConstantsRepr>(), 8);
        assert_eq!(align_of::<GraphicsPushConstantsRepr>(), 4);
        assert_eq!(offset_of!(GraphicsPushConstantsRepr, size), 0);
        assert_eq!(offset_of!(GraphicsPushConstantsRepr, stages), 4);
    }

    #[test]
    fn vertex_input_binding_repr_layout() {
        assert_eq!(size_of::<VertexInputBindingRepr>(), 16);
        assert_eq!(align_of::<VertexInputBindingRepr>(), 4);
        assert_eq!(offset_of!(VertexInputBindingRepr, binding), 0);
        assert_eq!(offset_of!(VertexInputBindingRepr, stride), 4);
        assert_eq!(offset_of!(VertexInputBindingRepr, input_rate), 8);
        assert_eq!(offset_of!(VertexInputBindingRepr, _reserved_padding), 12);
    }

    #[test]
    fn vertex_input_attribute_repr_layout() {
        assert_eq!(size_of::<VertexInputAttributeRepr>(), 16);
        assert_eq!(align_of::<VertexInputAttributeRepr>(), 4);
        assert_eq!(offset_of!(VertexInputAttributeRepr, location), 0);
        assert_eq!(offset_of!(VertexInputAttributeRepr, binding), 4);
        assert_eq!(offset_of!(VertexInputAttributeRepr, format), 8);
        assert_eq!(offset_of!(VertexInputAttributeRepr, offset), 12);
    }

    #[test]
    fn vertex_input_state_repr_layout() {
        // u32 + u32 + 2 (ptr,len) pairs = 8 + 32 = 40 bytes.
        assert_eq!(size_of::<VertexInputStateRepr>(), 40);
        assert_eq!(align_of::<VertexInputStateRepr>(), 8);
        assert_eq!(offset_of!(VertexInputStateRepr, kind), 0);
        assert_eq!(offset_of!(VertexInputStateRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(VertexInputStateRepr, bindings_ptr), 8);
        assert_eq!(offset_of!(VertexInputStateRepr, bindings_len), 16);
        assert_eq!(offset_of!(VertexInputStateRepr, attributes_ptr), 24);
        assert_eq!(offset_of!(VertexInputStateRepr, attributes_len), 32);
    }

    #[test]
    fn rasterization_state_repr_layout() {
        assert_eq!(size_of::<RasterizationStateRepr>(), 16);
        assert_eq!(align_of::<RasterizationStateRepr>(), 4);
        assert_eq!(offset_of!(RasterizationStateRepr, polygon_mode), 0);
        assert_eq!(offset_of!(RasterizationStateRepr, cull_mode), 4);
        assert_eq!(offset_of!(RasterizationStateRepr, front_face), 8);
        assert_eq!(offset_of!(RasterizationStateRepr, line_width), 12);
    }

    #[test]
    fn multisample_state_repr_layout() {
        assert_eq!(size_of::<MultisampleStateRepr>(), 8);
        assert_eq!(align_of::<MultisampleStateRepr>(), 4);
        assert_eq!(offset_of!(MultisampleStateRepr, samples), 0);
        assert_eq!(offset_of!(MultisampleStateRepr, _reserved_padding), 4);
    }

    #[test]
    fn depth_stencil_state_repr_layout() {
        assert_eq!(size_of::<DepthStencilStateRepr>(), 16);
        assert_eq!(align_of::<DepthStencilStateRepr>(), 4);
        assert_eq!(offset_of!(DepthStencilStateRepr, kind), 0);
        assert_eq!(offset_of!(DepthStencilStateRepr, depth_test), 4);
        assert_eq!(offset_of!(DepthStencilStateRepr, depth_write), 8);
        assert_eq!(offset_of!(DepthStencilStateRepr, _reserved_padding), 12);
    }

    #[test]
    fn color_blend_attachment_repr_layout() {
        assert_eq!(size_of::<ColorBlendAttachmentRepr>(), 32);
        assert_eq!(align_of::<ColorBlendAttachmentRepr>(), 4);
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, src_color_blend_factor),
            0
        );
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, dst_color_blend_factor),
            4
        );
        assert_eq!(offset_of!(ColorBlendAttachmentRepr, color_blend_op), 8);
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, src_alpha_blend_factor),
            12
        );
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, dst_alpha_blend_factor),
            16
        );
        assert_eq!(offset_of!(ColorBlendAttachmentRepr, alpha_blend_op), 20);
        assert_eq!(offset_of!(ColorBlendAttachmentRepr, color_write_mask), 24);
        assert_eq!(offset_of!(ColorBlendAttachmentRepr, _reserved_padding), 28);
    }

    #[test]
    fn color_blend_state_repr_layout() {
        // u32 + u32 + ColorBlendAttachmentRepr(32) = 40 bytes.
        assert_eq!(size_of::<ColorBlendStateRepr>(), 40);
        assert_eq!(align_of::<ColorBlendStateRepr>(), 4);
        assert_eq!(offset_of!(ColorBlendStateRepr, kind), 0);
        assert_eq!(offset_of!(ColorBlendStateRepr, color_write_mask), 4);
        assert_eq!(offset_of!(ColorBlendStateRepr, attachment), 8);
    }

    #[test]
    fn attachment_formats_repr_layout() {
        // (ptr,len) pair + u32 + u32 = 16 + 8 = 24 bytes.
        assert_eq!(size_of::<AttachmentFormatsRepr>(), 24);
        assert_eq!(align_of::<AttachmentFormatsRepr>(), 8);
        assert_eq!(offset_of!(AttachmentFormatsRepr, color_ptr), 0);
        assert_eq!(offset_of!(AttachmentFormatsRepr, color_len), 8);
        assert_eq!(offset_of!(AttachmentFormatsRepr, has_depth), 16);
        assert_eq!(offset_of!(AttachmentFormatsRepr, depth), 20);
    }

    #[test]
    fn graphics_pipeline_state_repr_layout() {
        // topology(4) + pad(4) + vertex_input(40) + raster(16) +
        // multisample(8) + depth_stencil(16) + color_blend(40) +
        // attachment_formats(24) + dynamic_state(4) + pad(4) = 160 bytes.
        assert_eq!(size_of::<GraphicsPipelineStateRepr>(), 160);
        assert_eq!(align_of::<GraphicsPipelineStateRepr>(), 8);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, topology), 0);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, _reserved_padding1), 4);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, vertex_input), 8);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, rasterization), 48);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, multisample), 64);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, depth_stencil), 72);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, color_blend), 88);
        assert_eq!(
            offset_of!(GraphicsPipelineStateRepr, attachment_formats),
            128
        );
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, dynamic_state), 152);
        assert_eq!(
            offset_of!(GraphicsPipelineStateRepr, _reserved_padding2),
            156
        );
    }

    #[test]
    fn graphics_kernel_descriptor_repr_layout() {
        // label(ptr,len)=16 + stages(ptr,len)=16 + bindings(ptr,len)=16
        // + push_constants(8) + pipeline_state(160) +
        // descriptor_sets_in_flight(4) + pad(4) = 224 bytes.
        assert_eq!(size_of::<GraphicsKernelDescriptorRepr>(), 224);
        assert_eq!(align_of::<GraphicsKernelDescriptorRepr>(), 8);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, label_ptr), 0);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, label_len), 8);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, stages_ptr), 16);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, stages_len), 24);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, bindings_ptr), 32);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, bindings_len), 40);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, push_constants), 48);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, pipeline_state), 56);
        assert_eq!(
            offset_of!(GraphicsKernelDescriptorRepr, descriptor_sets_in_flight),
            216
        );
        assert_eq!(
            offset_of!(GraphicsKernelDescriptorRepr, _reserved_padding),
            220
        );
    }

    #[test]
    fn viewport_repr_layout() {
        // 6 f32 fields, tightly packed, align 4.
        assert_eq!(size_of::<ViewportRepr>(), 24);
        assert_eq!(align_of::<ViewportRepr>(), 4);
        assert_eq!(offset_of!(ViewportRepr, x), 0);
        assert_eq!(offset_of!(ViewportRepr, y), 4);
        assert_eq!(offset_of!(ViewportRepr, width), 8);
        assert_eq!(offset_of!(ViewportRepr, height), 12);
        assert_eq!(offset_of!(ViewportRepr, min_depth), 16);
        assert_eq!(offset_of!(ViewportRepr, max_depth), 20);
    }

    #[test]
    fn scissor_rect_repr_layout() {
        // i32 + i32 + u32 + u32 = 16 bytes, align 4.
        assert_eq!(size_of::<ScissorRectRepr>(), 16);
        assert_eq!(align_of::<ScissorRectRepr>(), 4);
        assert_eq!(offset_of!(ScissorRectRepr, x), 0);
        assert_eq!(offset_of!(ScissorRectRepr, y), 4);
        assert_eq!(offset_of!(ScissorRectRepr, width), 8);
        assert_eq!(offset_of!(ScissorRectRepr, height), 12);
    }

    #[test]
    fn draw_call_repr_layout() {
        // 4 u32 + 2 u32 (present flags) = 24 bytes header,
        // ViewportRepr (24) + ScissorRectRepr (16) = 40 bytes payload,
        // total = 64 bytes, align 4 (all sub-fields ≤4-byte aligned).
        assert_eq!(size_of::<DrawCallRepr>(), 64);
        assert_eq!(align_of::<DrawCallRepr>(), 4);
        assert_eq!(offset_of!(DrawCallRepr, vertex_count), 0);
        assert_eq!(offset_of!(DrawCallRepr, instance_count), 4);
        assert_eq!(offset_of!(DrawCallRepr, first_vertex), 8);
        assert_eq!(offset_of!(DrawCallRepr, first_instance), 12);
        assert_eq!(offset_of!(DrawCallRepr, viewport_present), 16);
        assert_eq!(offset_of!(DrawCallRepr, scissor_present), 20);
        assert_eq!(offset_of!(DrawCallRepr, viewport), 24);
        assert_eq!(offset_of!(DrawCallRepr, scissor), 48);
    }

    #[test]
    fn draw_indexed_call_repr_layout() {
        // 5 u32 + 2 u32 (present) + 1 u32 (padding) = 32 bytes header,
        // ViewportRepr (24) + ScissorRectRepr (16) = 40 bytes payload,
        // total = 72 bytes, align 4.
        assert_eq!(size_of::<DrawIndexedCallRepr>(), 72);
        assert_eq!(align_of::<DrawIndexedCallRepr>(), 4);
        assert_eq!(offset_of!(DrawIndexedCallRepr, index_count), 0);
        assert_eq!(offset_of!(DrawIndexedCallRepr, instance_count), 4);
        assert_eq!(offset_of!(DrawIndexedCallRepr, first_index), 8);
        assert_eq!(offset_of!(DrawIndexedCallRepr, vertex_offset), 12);
        assert_eq!(offset_of!(DrawIndexedCallRepr, first_instance), 16);
        assert_eq!(offset_of!(DrawIndexedCallRepr, viewport_present), 20);
        assert_eq!(offset_of!(DrawIndexedCallRepr, scissor_present), 24);
        assert_eq!(offset_of!(DrawIndexedCallRepr, _reserved_padding), 28);
        assert_eq!(offset_of!(DrawIndexedCallRepr, viewport), 32);
        assert_eq!(offset_of!(DrawIndexedCallRepr, scissor), 56);
    }

    #[test]
    fn offscreen_draw_repr_layout() {
        // kind (u32) + _reserved_padding (u32) = 8 bytes header,
        // DrawCallRepr (64) + DrawIndexedCallRepr (72) = 136 bytes
        // payload, total = 144 bytes, align 4.
        assert_eq!(size_of::<OffscreenDrawRepr>(), 144);
        assert_eq!(align_of::<OffscreenDrawRepr>(), 4);
        assert_eq!(offset_of!(OffscreenDrawRepr, kind), 0);
        assert_eq!(offset_of!(OffscreenDrawRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(OffscreenDrawRepr, draw_call), 8);
        assert_eq!(offset_of!(OffscreenDrawRepr, draw_indexed_call), 72);
    }
}
