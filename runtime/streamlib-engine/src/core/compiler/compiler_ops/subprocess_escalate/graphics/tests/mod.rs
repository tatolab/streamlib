// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Host-Rust unit tests for the `register_graphics_kernel` /
//! `run_graphics_draw` escalate handlers.
//!
//! Mirrors `compute_kernel_dispatch`: the binding planner and the wire→RHI
//! pipeline-state translation are pure functions that run everywhere CI
//! does, and only the tests that build a real pipeline need a device.

use super::super::handle_escalate_op;
use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::super::kernel_shader_stage_source::{SPIRV_MAGIC_LE, registered_shader_stage_source};
use super::super::surface_bound_kernel_binding::{
    DeclaredKernelBindingUnderPlanning, SuppliedKernelBindingUnderPlanning,
    plan_supplied_surface_bound_kernel_bindings,
};
use super::linux_and_macos::{
    graphics_binding_kind_to_wire, graphics_pipeline_state_from_wire,
    surface_bound_graphics_binding_kind,
};
use crate::core::compiler::compiler_ops::subprocess_escalate::WhatTheNodeOfAHelperPlacedProcessorReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateGraphicsBindingKind, EscalateRequestRegisterGraphicsKernel,
    EscalateRequestRegisterGraphicsKernelBinding,
    EscalateRequestRegisterGraphicsKernelPipelineState,
    EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateTopology,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate,
    EscalateRequestRunGraphicsDraw, EscalateRequestRunGraphicsDrawBinding,
    EscalateRequestRunGraphicsDrawDraw, EscalateRequestRunGraphicsDrawDrawKind,
    EscalateRequestRunGraphicsDrawIndexBuffer, EscalateRequestRunGraphicsDrawIndexBufferIndexType,
    EscalateRequestRunGraphicsDrawScissor, EscalateRequestRunGraphicsDrawVertexBuffer,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
    EscalateRequest, EscalateResponse,
};
use crate::core::context::{GpuContext, GpuContextLimitedAccess, TexturePoolDescriptor};
use crate::core::rhi::{
    GraphicsBindingKind, PixelFormat, ShaderCompilationTargetStage, SurfaceBoundKernelBindingKind,
    TextureFormat, TextureUsages,
};

mod binding_planner_tests;
mod graphics_draw_rendering_and_synchronisation_tests;
mod graphics_kernel_registration_and_refusal_tests;
mod wire_pipeline_state_to_rhi_tests;

/// Graphics is an always-present capability now, so there is no bridge
/// to install — only a device to have or not have.
fn make_gpu_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
    GpuContext::init_for_platform_sync()
        .ok()
        .map(GpuContextLimitedAccess::new)
}

fn refusal_message(response: EscalateResponse) -> String {
    match response {
        EscalateResponse::Err(err) => err.message,
        other => panic!("expected Err, got {other:?}"),
    }
}

/// Fabricates a full-screen triangle out of the vertex index alone —
/// the only vertex source a draw over this op can have, since no
/// escalate op mints a vertex buffer.
const FULL_SCREEN_TRIANGLE_VERTEX_SLANG: &str = "\
float4 main(uint vertex_index : SV_VulkanVertexID) : SV_Position {
    float2 corner = float2((vertex_index << 1) & 2, vertex_index & 2);
    return float4(corner * 2.0 - 1.0, 0.0, 1.0);
}
";

/// Inverts the sampled input's colour and keeps its alpha, so the
/// rendered pixels prove which surface the named binding resolved to.
const INVERT_SAMPLED_INPUT_FRAGMENT_SLANG: &str = "\
[[vk::binding(0, 0)]] Sampler2D source_image;
float4 main(float4 fragment_position : SV_Position) : SV_Target {
    float4 source = source_image.Load(int3(int2(fragment_position.xy), 0));
    return float4(float3(1.0) - source.rgb, source.a);
}
";

/// The same pass with the fragment constant folded in, so registering
/// it produces a different pipeline — and therefore a different kernel
/// id — from [`INVERT_SAMPLED_INPUT_FRAGMENT_SLANG`].
const HALVE_SAMPLED_INPUT_FRAGMENT_SLANG: &str = "\
[[vk::binding(0, 0)]] Sampler2D source_image;
float4 main(float4 fragment_position : SV_Position) : SV_Target {
    float4 source = source_image.Load(int3(int2(fragment_position.xy), 0));
    return float4(source.rgb * 0.5, source.a);
}
";

/// Each seed channel inverts exactly in unorm8: out = 255 - in.
const SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
const INVERTED_RGBA: [u8; 4] = [245, 235, 225, 255];

/// Seeded into a colour target no draw fully covers: neither stage of
/// the kernel writes it, so a pixel still carrying it was loaded rather
/// than cleared.
const UNCOVERED_SENTINEL_RGBA: [u8; 4] = [3, 5, 7, 255];

/// What the handler's clear colour leaves in a pixel the draw missed.
const TRANSPARENT_BLACK_RGBA: [u8; 4] = [0, 0, 0, 0];

/// The baseline pipeline state every register request starts from —
/// TriangleList, no blending, no depth, one `rgba8_unorm` attachment.
fn baseline_pipeline_state() -> EscalateRequestRegisterGraphicsKernelPipelineState {
    EscalateRequestRegisterGraphicsKernelPipelineState {
        topology: EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleList,
        vertex_input_bindings: Vec::new(),
        vertex_input_attributes: Vec::new(),
        rasterization_polygon_mode:
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Fill,
        rasterization_cull_mode:
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::None,
        rasterization_front_face:
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::CounterClockwise,
        rasterization_line_width: 1.0,
        multisample_samples: 1,
        depth_stencil_enabled: false,
        depth_compare_op:
            EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp::Always,
        depth_write: false,
        color_blend_enabled: false,
        color_write_mask: 0b1111,
        color_blend_src_color_factor:
            EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor::One,
        color_blend_dst_color_factor:
            EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor::Zero,
        color_blend_color_op:
            EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp::Add,
        color_blend_src_alpha_factor:
            EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor::One,
        color_blend_dst_alpha_factor:
            EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor::Zero,
        color_blend_alpha_op:
            EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp::Add,
        attachment_color_formats: vec!["rgba8_unorm".to_string()],
        dynamic_state:
            EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::ViewportScissor,
        attachment_depth_format: None,
    }
}

/// A `register_graphics_kernel` request carrying pre-compiled SPIR-V
/// hex for both stages. Tests that need a specific shape mutate fields
/// after calling.
fn make_register_req(
    request_id: &str,
    vertex_hex: &str,
    fragment_hex: &str,
) -> EscalateRequestRegisterGraphicsKernel {
    EscalateRequestRegisterGraphicsKernel {
        fragment_source: "".to_string(),
        vertex_source: "".to_string(),
        request_id: request_id.to_string(),
        label: "test-graphics".to_string(),
        vertex_spv_hex: vertex_hex.to_string(),
        fragment_spv_hex: fragment_hex.to_string(),
        vertex_entry_point: "main".to_string(),
        fragment_entry_point: "main".to_string(),
        bindings: Vec::new(),
        push_constant_size: 0,
        push_constant_stages: 0,
        descriptor_sets_in_flight: 2,
        pipeline_state: baseline_pipeline_state(),
    }
}

/// The same request built from Slang, which is what the wire carries.
fn register_from_slang(
    request_id: &str,
    fragment_source: &str,
) -> EscalateRequestRegisterGraphicsKernel {
    let mut req = make_register_req(request_id, "", "");
    req.vertex_source = FULL_SCREEN_TRIANGLE_VERTEX_SLANG.to_string();
    req.fragment_source = fragment_source.to_string();
    req
}

/// Baseline `run_graphics_draw` request — vertex-fabricating (no vertex
/// buffers, no index buffer), one colour target, a simple Draw of the
/// full-screen triangle's three vertices.
fn make_run_req(
    request_id: &str,
    kernel_id: &str,
    surface_uuid: &str,
) -> EscalateRequestRunGraphicsDraw {
    EscalateRequestRunGraphicsDraw {
        request_id: request_id.to_string(),
        kernel_id: kernel_id.to_string(),
        frame_index: 0,
        bindings: Vec::new(),
        vertex_buffers: Vec::new(),
        color_target_uuids: vec![surface_uuid.to_string()],
        extent_width: 320,
        extent_height: 240,
        push_constants_hex: String::new(),
        draw: EscalateRequestRunGraphicsDrawDraw {
            kind: EscalateRequestRunGraphicsDrawDrawKind::Draw,
            vertex_count: 3,
            index_count: 0,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
            first_index: 0,
            vertex_offset: 0,
        },
        index_buffer: None,
        depth_target_uuid: None,
        viewport: None,
        scissor: None,
    }
}

fn register_graphics_kernel_or_panic(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    req: EscalateRequestRegisterGraphicsKernel,
) -> EscalateResponseOk {
    let response = handle_escalate_op(
        sandbox,
        registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::RegisterGraphicsKernel(req),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Ok(ok) => ok,
        other => panic!("registering the graphics kernel failed: {other:?}"),
    }
}
