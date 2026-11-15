// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The wire's graphics pipeline state translated into the RHI's.

use super::*;

/// Lock in the wire→RHI pipeline-state translation. Mentally reverting
/// any single arm of `graphics_pipeline_state_from_wire` (e.g. swapping
/// `Add ↔ Subtract`) must fail this test — nothing else checks the
/// ~200 lines of enum mapping in the handler, and a wrong arm builds a
/// pipeline the caller did not ask for without complaint.
#[test]
fn pipeline_state_translates_every_enum_arm() {
    use crate::core::rhi::{
        BlendFactor, BlendOp, ColorBlendState, ColorWriteMask, CullMode, DepthStencilState,
        FrontFace, GraphicsDynamicState, PolygonMode, PrimitiveTopology, VertexInputState,
    };

    // Every value is chosen to differ from the matching default, so a
    // wrong arm in the translation lands in the wrong RHI variant and
    // the assertion fails.
    let mut wire = baseline_pipeline_state();
    wire.topology = EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleStrip;
    wire.rasterization_polygon_mode =
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Line;
    wire.rasterization_cull_mode =
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Back;
    wire.rasterization_front_face =
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::Clockwise;
    wire.rasterization_line_width = 2.5;
    wire.color_blend_enabled = true;
    wire.color_write_mask = 0b0101; // R | B only
    wire.color_blend_src_color_factor =
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor::SrcAlpha;
    wire.color_blend_dst_color_factor =
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor::OneMinusSrcAlpha;
    wire.color_blend_color_op =
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp::Subtract;
    wire.color_blend_src_alpha_factor =
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor::ConstantAlpha;
    wire.color_blend_dst_alpha_factor =
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor::OneMinusConstantAlpha;
    wire.color_blend_alpha_op =
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp::Max;
    wire.attachment_color_formats = vec!["bgra8_unorm_srgb".to_string()];
    wire.dynamic_state = EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::None;

    let state = graphics_pipeline_state_from_wire(wire).expect("a buildable shape");

    assert_eq!(state.topology, PrimitiveTopology::TriangleStrip);
    // Not a translated arm: both halves of a vertex input are refused
    // below, so the only vertex-input state this can produce is the
    // gl_VertexIndex-driven one.
    assert!(
        matches!(state.vertex_input, VertexInputState::None),
        "expected the gl_VertexIndex-driven shape, got {:?}",
        state.vertex_input
    );
    assert_eq!(state.rasterization.polygon_mode, PolygonMode::Line);
    assert_eq!(state.rasterization.cull_mode, CullMode::Back);
    assert_eq!(state.rasterization.front_face, FrontFace::Clockwise);
    assert_eq!(state.rasterization.line_width, 2.5);
    assert_eq!(state.multisample.samples, 1);
    // Not a translated arm: both halves of a depth attachment are
    // refused above, so the only depth state this can produce is off.
    assert_eq!(state.depth_stencil, DepthStencilState::Disabled);
    match state.color_blend {
        ColorBlendState::Enabled(attachment) => {
            assert_eq!(attachment.src_color_blend_factor, BlendFactor::SrcAlpha);
            assert_eq!(
                attachment.dst_color_blend_factor,
                BlendFactor::OneMinusSrcAlpha
            );
            assert_eq!(attachment.color_blend_op, BlendOp::Subtract);
            assert_eq!(
                attachment.src_alpha_blend_factor,
                BlendFactor::ConstantAlpha
            );
            assert_eq!(
                attachment.dst_alpha_blend_factor,
                BlendFactor::OneMinusConstantAlpha
            );
            assert_eq!(attachment.alpha_blend_op, BlendOp::Max);
            assert_eq!(
                attachment.color_write_mask,
                ColorWriteMask::R | ColorWriteMask::B
            );
        }
        other => panic!("expected blending on, got {other:?}"),
    }
    assert_eq!(
        state.attachment_formats.color,
        vec![TextureFormat::Bgra8UnormSrgb]
    );
    assert_eq!(state.attachment_formats.depth, None);
    assert_eq!(state.dynamic_state, GraphicsDynamicState::None);
}

/// The four blend-factor fields and the two blend-op fields share one
/// macro each, so a swapped arm there is wrong in every field at once
/// and the single-value test above would only catch the arm it picked.
#[test]
fn every_blend_factor_and_blend_op_arm_reaches_the_rhi_attachment() {
    use {
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp as AlphaOpWire,
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp as ColorOpWire,
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor as DstAlphaWire,
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor as DstColorWire,
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor as SrcAlphaWire,
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor as SrcColorWire,
    };

    use crate::core::rhi::{BlendFactor, BlendOp, ColorBlendState};

    let factor_arms = [
        (
            SrcColorWire::Zero,
            DstColorWire::Zero,
            SrcAlphaWire::Zero,
            DstAlphaWire::Zero,
            BlendFactor::Zero,
        ),
        (
            SrcColorWire::One,
            DstColorWire::One,
            SrcAlphaWire::One,
            DstAlphaWire::One,
            BlendFactor::One,
        ),
        (
            SrcColorWire::SrcColor,
            DstColorWire::SrcColor,
            SrcAlphaWire::SrcColor,
            DstAlphaWire::SrcColor,
            BlendFactor::SrcColor,
        ),
        (
            SrcColorWire::OneMinusSrcColor,
            DstColorWire::OneMinusSrcColor,
            SrcAlphaWire::OneMinusSrcColor,
            DstAlphaWire::OneMinusSrcColor,
            BlendFactor::OneMinusSrcColor,
        ),
        (
            SrcColorWire::DstColor,
            DstColorWire::DstColor,
            SrcAlphaWire::DstColor,
            DstAlphaWire::DstColor,
            BlendFactor::DstColor,
        ),
        (
            SrcColorWire::OneMinusDstColor,
            DstColorWire::OneMinusDstColor,
            SrcAlphaWire::OneMinusDstColor,
            DstAlphaWire::OneMinusDstColor,
            BlendFactor::OneMinusDstColor,
        ),
        (
            SrcColorWire::SrcAlpha,
            DstColorWire::SrcAlpha,
            SrcAlphaWire::SrcAlpha,
            DstAlphaWire::SrcAlpha,
            BlendFactor::SrcAlpha,
        ),
        (
            SrcColorWire::OneMinusSrcAlpha,
            DstColorWire::OneMinusSrcAlpha,
            SrcAlphaWire::OneMinusSrcAlpha,
            DstAlphaWire::OneMinusSrcAlpha,
            BlendFactor::OneMinusSrcAlpha,
        ),
        (
            SrcColorWire::DstAlpha,
            DstColorWire::DstAlpha,
            SrcAlphaWire::DstAlpha,
            DstAlphaWire::DstAlpha,
            BlendFactor::DstAlpha,
        ),
        (
            SrcColorWire::OneMinusDstAlpha,
            DstColorWire::OneMinusDstAlpha,
            SrcAlphaWire::OneMinusDstAlpha,
            DstAlphaWire::OneMinusDstAlpha,
            BlendFactor::OneMinusDstAlpha,
        ),
        (
            SrcColorWire::ConstantColor,
            DstColorWire::ConstantColor,
            SrcAlphaWire::ConstantColor,
            DstAlphaWire::ConstantColor,
            BlendFactor::ConstantColor,
        ),
        (
            SrcColorWire::OneMinusConstantColor,
            DstColorWire::OneMinusConstantColor,
            SrcAlphaWire::OneMinusConstantColor,
            DstAlphaWire::OneMinusConstantColor,
            BlendFactor::OneMinusConstantColor,
        ),
        (
            SrcColorWire::ConstantAlpha,
            DstColorWire::ConstantAlpha,
            SrcAlphaWire::ConstantAlpha,
            DstAlphaWire::ConstantAlpha,
            BlendFactor::ConstantAlpha,
        ),
        (
            SrcColorWire::OneMinusConstantAlpha,
            DstColorWire::OneMinusConstantAlpha,
            SrcAlphaWire::OneMinusConstantAlpha,
            DstAlphaWire::OneMinusConstantAlpha,
            BlendFactor::OneMinusConstantAlpha,
        ),
        (
            SrcColorWire::SrcAlphaSaturate,
            DstColorWire::SrcAlphaSaturate,
            SrcAlphaWire::SrcAlphaSaturate,
            DstAlphaWire::SrcAlphaSaturate,
            BlendFactor::SrcAlphaSaturate,
        ),
    ];
    for (src_color, dst_color, src_alpha, dst_alpha, expected) in factor_arms {
        let mut wire = baseline_pipeline_state();
        wire.color_blend_enabled = true;
        wire.color_blend_src_color_factor = src_color;
        wire.color_blend_dst_color_factor = dst_color;
        wire.color_blend_src_alpha_factor = src_alpha;
        wire.color_blend_dst_alpha_factor = dst_alpha;
        let state = graphics_pipeline_state_from_wire(wire).expect("a buildable shape");
        match state.color_blend {
            ColorBlendState::Enabled(attachment) => {
                assert_eq!(attachment.src_color_blend_factor, expected);
                assert_eq!(attachment.dst_color_blend_factor, expected);
                assert_eq!(attachment.src_alpha_blend_factor, expected);
                assert_eq!(attachment.dst_alpha_blend_factor, expected);
            }
            other => panic!("expected blending on, got {other:?}"),
        }
    }

    let op_arms = [
        (ColorOpWire::Add, AlphaOpWire::Add, BlendOp::Add),
        (
            ColorOpWire::Subtract,
            AlphaOpWire::Subtract,
            BlendOp::Subtract,
        ),
        (
            ColorOpWire::ReverseSubtract,
            AlphaOpWire::ReverseSubtract,
            BlendOp::ReverseSubtract,
        ),
        (ColorOpWire::Min, AlphaOpWire::Min, BlendOp::Min),
        (ColorOpWire::Max, AlphaOpWire::Max, BlendOp::Max),
    ];
    for (color_op, alpha_op, expected) in op_arms {
        let mut wire = baseline_pipeline_state();
        wire.color_blend_enabled = true;
        wire.color_blend_color_op = color_op;
        wire.color_blend_alpha_op = alpha_op;
        let state = graphics_pipeline_state_from_wire(wire).expect("a buildable shape");
        match state.color_blend {
            ColorBlendState::Enabled(attachment) => {
                assert_eq!(attachment.color_blend_op, expected);
                assert_eq!(attachment.alpha_blend_op, expected);
            }
            other => panic!("expected blending on, got {other:?}"),
        }
    }
}

/// The wire promises these refusals and nothing downstream enforces
/// them: an MSAA pipeline, a multi-attachment one, and either half of a
/// depth attachment or of a vertex input are shapes a draw over this op
/// has no path for.
#[test]
fn a_pipeline_state_the_kernel_cannot_build_is_refused() {
    let mut multisampled = baseline_pipeline_state();
    multisampled.multisample_samples = 4;
    let message =
        graphics_pipeline_state_from_wire(multisampled).expect_err("MSAA must be refused");
    assert!(message.contains("single-sampled"), "{message}");

    let mut two_attachments = baseline_pipeline_state();
    two_attachments.attachment_color_formats =
        vec!["rgba8_unorm".to_string(), "rgba8_unorm".to_string()];
    let message = graphics_pipeline_state_from_wire(two_attachments)
        .expect_err("two colour attachments must be refused");
    assert!(message.contains("exactly one"), "{message}");

    // The draw op refuses `depth_target_uuid` for the same reason; a
    // pipeline built with depth state would otherwise disagree with the
    // colour-only pass at every draw, a submission away from its cause.
    let mut depth_testing = baseline_pipeline_state();
    depth_testing.depth_stencil_enabled = true;
    let message = graphics_pipeline_state_from_wire(depth_testing)
        .expect_err("depth testing must be refused");
    assert!(message.contains("colour targets only"), "{message}");

    let mut depth_attachment = baseline_pipeline_state();
    depth_attachment.attachment_depth_format =
        Some(EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat::D32Sfloat);
    let message = graphics_pipeline_state_from_wire(depth_attachment)
        .expect_err("a depth attachment must be refused");
    assert!(message.contains("colour targets only"), "{message}");

    let mut unowned_write_mask = baseline_pipeline_state();
    unowned_write_mask.color_write_mask = 0b1_0000;
    let message = graphics_pipeline_state_from_wire(unowned_write_mask)
        .expect_err("a bit no channel owns must be refused");
    assert!(message.contains("no colour channel owns"), "{message}");

    // The draw op refuses `vertex_buffers` for the same reason. A
    // pipeline pulling from a vertex binding would otherwise register
    // and then be refused at every draw, for a buffer no escalate op
    // can mint to fill it.
    let mut buffer_fed_vertices = baseline_pipeline_state();
    buffer_fed_vertices.vertex_input_bindings = vec![
        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding {
            binding: 0,
            stride: 12,
            input_rate:
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate::Vertex,
        },
    ];
    let message = graphics_pipeline_state_from_wire(buffer_fed_vertices)
        .expect_err("a vertex binding no buffer can fill must be refused");
    assert!(
        message.contains("no escalate op mints a VertexBuffer"),
        "{message}"
    );
    assert!(message.contains("gl_VertexIndex"), "{message}");

    let mut unfed_attributes = baseline_pipeline_state();
    unfed_attributes.vertex_input_attributes = vec![
        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute {
            location: 0,
            binding: 0,
            format:
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat::Rgb32Float,
            offset: 0,
        },
    ];
    let message = graphics_pipeline_state_from_wire(unfed_attributes)
        .expect_err("an attribute with no binding it could be fed from must be refused");
    assert!(message.contains("pulled from a"), "{message}");
    assert!(message.contains("gl_VertexIndex"), "{message}");
}
