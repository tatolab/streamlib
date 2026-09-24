// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
    GlslCompilationTargetStage, GraphicsBindingKind, PixelFormat, SurfaceBoundKernelBindingKind,
    TextureFormat, TextureUsages,
};
use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

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

/// Fabricates a full-screen triangle out of `gl_VertexIndex` alone —
/// the only vertex source a draw over this op can have, since no
/// escalate op mints a vertex buffer.
const FULL_SCREEN_TRIANGLE_VERTEX_GLSL: &str = "\
#version 450
void main() {
    vec2 corner = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
";

/// Inverts the sampled input's colour and keeps its alpha, so the
/// rendered pixels prove which surface the named binding resolved to.
const INVERT_SAMPLED_INPUT_FRAGMENT_GLSL: &str = "\
#version 450
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 painted_colour;
void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    painted_colour = vec4(vec3(1.0) - source.rgb, source.a);
}
";

/// The same pass with the fragment constant folded in, so registering
/// it produces a different pipeline — and therefore a different kernel
/// id — from [`INVERT_SAMPLED_INPUT_FRAGMENT_GLSL`].
const HALVE_SAMPLED_INPUT_FRAGMENT_GLSL: &str = "\
#version 450
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 painted_colour;
void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    painted_colour = vec4(source.rgb * 0.5, source.a);
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

/// The same request built from GLSL, which is what the wire carries
/// now that the engine owns compilation.
fn register_from_glsl(
    request_id: &str,
    fragment_source: &str,
) -> EscalateRequestRegisterGraphicsKernel {
    let mut req = make_register_req(request_id, "", "");
    req.vertex_source = FULL_SCREEN_TRIANGLE_VERTEX_GLSL.to_string();
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
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RegisterGraphicsKernel(req),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Ok(ok) => ok,
        other => panic!("registering the graphics kernel failed: {other:?}"),
    }
}

// ----- the binding planner --------------------------------------
//
// Pure wire validation shared with the trace path, driven here through
// the graphics kinds: no device, so these run everywhere CI does.

fn declared_graphics_bindings(
    entries: &'static [(u32, &'static str, GraphicsBindingKind)],
) -> Vec<DeclaredKernelBindingUnderPlanning<'static>> {
    entries
        .iter()
        .map(
            |(binding_slot, name, kind)| DeclaredKernelBindingUnderPlanning {
                binding_slot: *binding_slot,
                name: Some(name),
                kind_wire_name: graphics_binding_kind_to_wire(*kind).wire_name(),
                surface_bound_kind: surface_bound_graphics_binding_kind(*kind),
            },
        )
        .collect()
}

/// The shape the planner tests measure against: one sampled input and
/// one storage output, deliberately different kinds so binding by slot
/// order rather than by name would swap them.
const A_DRAWING_KERNELS_BINDINGS: &[(u32, &str, GraphicsBindingKind)] = &[
    (0, "source_image", GraphicsBindingKind::SampledTexture),
    (1, "painted_output", GraphicsBindingKind::StorageImage),
];

/// A kernel whose one binding is a uniform buffer — a kind a draw
/// cannot name a surface for.
const A_TINTING_KERNELS_BINDINGS: &[(u32, &str, GraphicsBindingKind)] =
    &[(0, "tint_parameters", GraphicsBindingKind::UniformBuffer)];

fn supplied_graphics_bindings<'a>(
    entries: &'a [(&'a str, EscalateGraphicsBindingKind, &'a str)],
) -> Vec<SuppliedKernelBindingUnderPlanning<'a>> {
    entries
        .iter()
        .map(
            |(name, kind, target_id)| SuppliedKernelBindingUnderPlanning {
                name,
                target_id,
                kind_wire_name: kind.wire_name(),
            },
        )
        .collect()
}

fn draw_plan_refusal(
    declared: &'static [(u32, &'static str, GraphicsBindingKind)],
    supplied: &[(&str, EscalateGraphicsBindingKind, &str)],
) -> String {
    let declared = declared_graphics_bindings(declared);
    let supplied = supplied_graphics_bindings(supplied);
    plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)
        .err()
        .expect("expected the plan to be refused")
        .to_string()
}

#[test]
fn a_complete_draw_resolves_every_name_to_its_slot() {
    let declared = declared_graphics_bindings(A_DRAWING_KERNELS_BINDINGS);
    let supplied = supplied_graphics_bindings(&[
        (
            "painted_output",
            EscalateGraphicsBindingKind::StorageImage,
            "surface-out",
        ),
        (
            "source_image",
            EscalateGraphicsBindingKind::SampledTexture,
            "surface-in",
        ),
    ]);
    let planned = plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)
        .expect("a complete, correctly-typed draw");

    // Resolution is by name, so the order the caller supplied them in
    // is not the order the shaders declared them in — and that is fine.
    assert_eq!(planned.len(), 2);
    assert_eq!(planned[0].name, "painted_output");
    assert_eq!(planned[0].binding_slot, 1);
    assert_eq!(planned[0].kind, SurfaceBoundKernelBindingKind::StorageImage);
    assert_eq!(planned[0].target_id, "surface-out");
    assert_eq!(planned[1].name, "source_image");
    assert_eq!(planned[1].binding_slot, 0);
    assert_eq!(
        planned[1].kind,
        SurfaceBoundKernelBindingKind::SampledTexture
    );
}

/// Not expressible in a Python mapping — a dict cannot carry one key
/// twice — so the wire array is the only layer that can guard it.
#[test]
fn a_name_supplied_twice_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-in",
            ),
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-other",
            ),
            (
                "painted_output",
                EscalateGraphicsBindingKind::StorageImage,
                "surface-out",
            ),
        ],
    );
    assert!(
        message.contains("binding `source_image` was supplied twice"),
        "must name the duplicate, got: {message}"
    );
    assert!(
        message.contains("`source_image`, `painted_output`"),
        "must name the kernel's declared bindings, got: {message}"
    );
}

#[test]
fn a_name_the_shaders_do_not_declare_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-in",
            ),
            (
                "painted_output",
                EscalateGraphicsBindingKind::StorageImage,
                "surface-out",
            ),
            (
                "sharpen_amount",
                EscalateGraphicsBindingKind::UniformBuffer,
                "surface-x",
            ),
        ],
    );
    assert!(
        message.contains("binding `sharpen_amount` is not one this kernel declares"),
        "must name the unknown binding, got: {message}"
    );
    assert!(
        message.contains("`source_image`, `painted_output`"),
        "must name the kernel's declared bindings, got: {message}"
    );
}

/// No implicit default and no carried-over value: the kernel holds no
/// binding state between draws to fall back on.
#[test]
fn a_declared_binding_left_out_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[(
            "source_image",
            EscalateGraphicsBindingKind::SampledTexture,
            "surface-in",
        )],
    );
    assert!(
        message.contains("binding `painted_output` was not supplied"),
        "must name the missing binding, got: {message}"
    );
    assert!(
        message.contains("do not persist between draws"),
        "must say why there is no fallback, got: {message}"
    );
}

#[test]
fn a_binding_supplied_as_the_wrong_kind_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-in",
            ),
            (
                "painted_output",
                EscalateGraphicsBindingKind::StorageBuffer,
                "surface-out",
            ),
        ],
    );
    assert!(
        message.contains("binding `painted_output` was supplied as storage_buffer"),
        "must name the binding and the kind supplied, got: {message}"
    );
    assert!(
        message.contains("declares it storage_image"),
        "must name the kind the kernel declares, got: {message}"
    );
}

/// A buffer binding is legal in a shader and legal on the wire, but no
/// escalate op mints a buffer a descriptor can point at — so the draw
/// that would need one is refused rather than silently unbound.
#[test]
fn a_binding_of_a_kind_no_surface_can_back_is_refused() {
    let message = draw_plan_refusal(
        A_TINTING_KERNELS_BINDINGS,
        &[(
            "tint_parameters",
            EscalateGraphicsBindingKind::UniformBuffer,
            "surface-x",
        )],
    );
    assert!(
        message.contains("binding `tint_parameters` is uniform_buffer"),
        "must name the binding and its kind, got: {message}"
    );
    assert!(
        message.contains("storage_image and sampled_texture"),
        "must name the kinds a draw can bind, got: {message}"
    );
}

/// A kernel with no bindings at all draws — the empty case is not an
/// error, and the "missing" rule has nothing to fire on.
#[test]
fn a_kernel_declaring_nothing_needs_nothing_supplied() {
    let planned = plan_supplied_surface_bound_kernel_bindings("draw", &[], &[])
        .expect("an unbound kernel draws");
    assert!(planned.is_empty());
}

// ----- wire → RHI pipeline state --------------------------------

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
    let message = graphics_pipeline_state_from_wire(multisampled)
        .err()
        .expect("MSAA must be refused");
    assert!(message.contains("single-sampled"), "{message}");

    let mut two_attachments = baseline_pipeline_state();
    two_attachments.attachment_color_formats =
        vec!["rgba8_unorm".to_string(), "rgba8_unorm".to_string()];
    let message = graphics_pipeline_state_from_wire(two_attachments)
        .err()
        .expect("two colour attachments must be refused");
    assert!(message.contains("exactly one"), "{message}");

    // The draw op refuses `depth_target_uuid` for the same reason; a
    // pipeline built with depth state would otherwise disagree with the
    // colour-only pass at every draw, a submission away from its cause.
    let mut depth_testing = baseline_pipeline_state();
    depth_testing.depth_stencil_enabled = true;
    let message = graphics_pipeline_state_from_wire(depth_testing)
        .err()
        .expect("depth testing must be refused");
    assert!(message.contains("colour targets only"), "{message}");

    let mut depth_attachment = baseline_pipeline_state();
    depth_attachment.attachment_depth_format =
        Some(EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat::D32Sfloat);
    let message = graphics_pipeline_state_from_wire(depth_attachment)
        .err()
        .expect("a depth attachment must be refused");
    assert!(message.contains("colour targets only"), "{message}");

    let mut unowned_write_mask = baseline_pipeline_state();
    unowned_write_mask.color_write_mask = 0b1_0000;
    let message = graphics_pipeline_state_from_wire(unowned_write_mask)
        .err()
        .expect("a bit no channel owns must be refused");
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
        .err()
        .expect("a vertex binding no buffer can fill must be refused");
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
        .err()
        .expect("an attribute with no binding it could be fed from must be refused");
    assert!(message.contains("pulled from a"), "{message}");
    assert!(message.contains("gl_VertexIndex"), "{message}");
}

// ----- the handlers ---------------------------------------------

/// Both hex fields are decoded before the escalate hop, so a malformed
/// one is refused without touching the GPU at all.
#[test]
fn register_with_invalid_vertex_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_with_invalid_vertex_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RegisterGraphicsKernel(make_register_req(
            "req-bad-v",
            "xyz123",
            "cafebabe",
        )),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, "req-bad-v");
            assert!(
                err.message.contains("vertex_spv_hex"),
                "got: {}",
                err.message
            );
        }
        other => panic!("expected Err for malformed vertex hex, got {other:?}"),
    }
}

#[test]
fn register_with_invalid_fragment_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_with_invalid_fragment_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RegisterGraphicsKernel(make_register_req("req-bad-f", "deadbeef", "qq")),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, "req-bad-f");
            assert!(
                err.message.contains("fragment_spv_hex"),
                "got: {}",
                err.message
            );
        }
        other => panic!("expected Err for malformed fragment hex, got {other:?}"),
    }
}

#[test]
fn run_with_invalid_push_constants_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("run_with_invalid_push_constants_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("req-bad-push", "kernel-x", "surface-y");
    req.push_constants_hex = "xyz".to_string();
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunGraphicsDraw(req),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, "req-bad-push");
            assert!(
                err.message.contains("push_constants_hex"),
                "got: {}",
                err.message
            );
        }
        other => panic!("expected Err for malformed push hex, got {other:?}"),
    }
}

/// The three shapes the wire carries that the host has no path for.
/// Each is refused rather than silently dropped: a caller who sent one
/// would otherwise get a draw that ignored half of what it asked for.
#[test]
fn a_draw_naming_a_resource_no_escalate_op_mints_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_naming_an_unmintable_resource: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();

    let mut with_vertex_buffer = make_run_req("req-vb", "kernel-x", "surface-y");
    with_vertex_buffer.vertex_buffers = vec![EscalateRequestRunGraphicsDrawVertexBuffer {
        binding: 0,
        surface_uuid: "vb-uuid".to_string(),
        offset: "128".to_string(),
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunGraphicsDraw(with_vertex_buffer),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("no escalate op mints a VertexBuffer"),
        "must say what is missing, got: {message}"
    );

    let mut indexed = make_run_req("req-ib", "kernel-x", "surface-y");
    indexed.index_buffer = Some(EscalateRequestRunGraphicsDrawIndexBuffer {
        surface_uuid: "ib-uuid".to_string(),
        offset: "64".to_string(),
        index_type: EscalateRequestRunGraphicsDrawIndexBufferIndexType::Uint32,
    });
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunGraphicsDraw(indexed),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("an indexed draw needs an IndexBuffer"),
        "must say what is missing, got: {message}"
    );

    // The index buffer is what a `draw_indexed` names its indices in,
    // so the draw kind alone is refused for the same reason.
    let mut indexed_without_a_buffer = make_run_req("req-ib-kind", "kernel-x", "surface-y");
    indexed_without_a_buffer.draw.kind = EscalateRequestRunGraphicsDrawDrawKind::DrawIndexed;
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunGraphicsDraw(indexed_without_a_buffer),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("an indexed draw needs an IndexBuffer"),
        "must say what is missing, got: {message}"
    );
}

/// The offscreen pass attaches colour targets only, so a depth target
/// would never be tested against — and a caller who set one is asking
/// for depth testing that would not happen.
#[test]
fn a_draw_naming_a_depth_target_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_naming_a_depth_target: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("req-depth", "kernel-x", "surface-y");
    req.depth_target_uuid = Some("depth-uuid".to_string());
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunGraphicsDraw(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("depth_target_uuid is set"),
        "must name the field, got: {message}"
    );
    assert!(
        message.contains("colour targets only"),
        "must say why, got: {message}"
    );
}

#[test]
fn a_draw_naming_other_than_one_colour_target_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_naming_other_than_one_colour_target: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("req-targets", "kernel-x", "surface-y");
    req.color_target_uuids = vec!["a".to_string(), "b".to_string()];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunGraphicsDraw(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("exactly one colour attachment"),
        "got: {message}"
    );
}

#[test]
fn drawing_with_an_unregistered_kernel_id_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("drawing_with_an_unregistered_kernel_id: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunGraphicsDraw(make_run_req(
                "req-bad-id",
                "never-registered",
                "surface-y",
            )),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("no kernel registered under id") && message.contains("never-registered"),
        "got: {message}"
    );
}

/// A stage mask is a bitfield the caller writes by hand, and a bit
/// outside vertex|fragment names a stage a graphics pipeline has no
/// module for at all.
#[test]
fn a_binding_declared_for_a_stage_no_graphics_pipeline_has_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_binding_declared_for_an_unowned_stage: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = register_from_glsl("req-stage", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL);
    req.bindings = vec![EscalateRequestRegisterGraphicsKernelBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        stages: 0b100,
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterGraphicsKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("no graphics stage owns"),
        "must say the bit belongs to no stage, got: {message}"
    );
}

/// GLSL where bytes used to go: the engine compiles each stage itself,
/// and what it hands the pipeline is a module rather than the text.
#[test]
fn glsl_for_each_stage_reaches_the_engine_as_compiled_spirv() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("glsl_for_each_stage_reaches_the_engine: no GPU — skipping");
        return;
    };
    for (field_prefix, source, stage) in [
        (
            "vertex_",
            FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
            GlslCompilationTargetStage::Vertex,
        ),
        (
            "fragment_",
            INVERT_SAMPLED_INPUT_FRAGMENT_GLSL,
            GlslCompilationTargetStage::Fragment,
        ),
    ] {
        let compiled = registered_shader_stage_source(field_prefix, source, "", stage, "")
            .expect("GLSL alone is one of the two alternatives")
            .spirv(&sandbox)
            .expect("the engine compiles it");
        assert_eq!(
            compiled.get(..4),
            Some(&SPIRV_MAGIC_LE[..]),
            "the {stage:?} stage reached the pipeline as something other than SPIR-V"
        );
    }
}

/// Registration hands back the shape a draw needs: the shaders' own
/// names, each with the kind only the shaders know. No bridge is
/// installed — graphics is a capability the context always has.
#[test]
fn registration_answers_with_the_shaders_binding_names_and_kinds() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("registration_answers_with_the_shaders_bindings: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let ok = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("reg", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
    );
    let bindings = ok.bindings.expect("a register response carries the shape");
    assert_eq!(
        bindings
            .iter()
            .map(|binding| (binding.name.as_str(), binding.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![("source_image", "sampled_texture")],
        "the fragment shader's own binding, named and kinded as it declares it"
    );
}

/// Re-registering an identical kernel is free and keeps its id; a
/// different fragment stage is a different pipeline and gets its own.
#[test]
fn an_identical_registration_keeps_its_kernel_id_and_a_different_one_gets_its_own() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("an_identical_registration_keeps_its_kernel_id: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let first = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("a", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
    )
    .handle_id;
    let second = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("b", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
    )
    .handle_id;
    assert_eq!(
        first, second,
        "an identical descriptor must produce the same kernel_id"
    );

    let other = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("c", HALVE_SAMPLED_INPUT_FRAGMENT_GLSL),
    )
    .handle_id;
    assert_ne!(
        first, other,
        "a different fragment stage must produce a different kernel_id"
    );

    let held = sandbox
        .escalate(|full| {
            Ok((
                full.graphics_kernel_by_id(&first),
                full.graphics_kernel_by_id(&second),
            ))
        })
        .expect("the cache answers inside an escalate scope");
    let (a, b) = (held.0.expect("cached"), held.1.expect("cached"));
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "the second registration must reuse the first kernel, not build another"
    );
}

/// The cache key covers the shaders and the pipeline, not the caller's
/// assertion — so a wrong declaration refuses identically whether or
/// not somebody registered this kernel first.
#[test]
fn a_wrong_declaration_is_refused_even_when_the_kernel_is_cached() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_wrong_declaration_is_refused_when_cached: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("warm", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
    );

    let mut req = register_from_glsl("reg-wrong", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL);
    req.bindings = vec![EscalateRequestRegisterGraphicsKernelBinding {
        kind: EscalateGraphicsBindingKind::StorageBuffer,
        name: "sharpen_amount".to_string(),
        stages: 0,
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterGraphicsKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("`sharpen_amount`") && message.contains("`source_image`"),
        "the refusal must name the bogus binding and the shaders' own: {message}"
    );
}

/// The op end to end, over a real device: a draw resolves its binding
/// by the fragment shader's own name, renders into the surface the
/// request named, and leaves the engine's layout record agreeing with
/// the layout the pass left the image in.
///
/// The source is seeded with a known value and the target is read back
/// and compared against the shader's own arithmetic, so a draw that
/// bound nothing — or bound the target to itself — fails on the pixels
/// rather than passing silently. The seeded source is then moved to
/// `GENERAL`, which a combined image sampler does not satisfy: a draw
/// that did not barrier its bound inputs would read it through a
/// descriptor its layout disagrees with, and would leave the engine's
/// record still saying `GENERAL`.
#[test]
fn a_draw_reads_the_surface_its_binding_names_and_publishes_the_targets_layout() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_reads_the_surface_its_binding_names: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("reg-draw", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
    )
    .handle_id;

    // Held for the draw: dropping a pooled handle hands its slot back,
    // and the registration would then name a recycled texture.
    let held = sandbox
        .escalate(|full| {
            let source = full.acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
            )?;
            let target = full.acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC),
            )?;
            full.register_texture("draw-source", source.texture().clone());
            full.register_texture("draw-target", target.texture().clone());

            let (_pool_id, seed_buffer) = full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;
            let plane = seed_buffer.buffer_ref().plane_base_address(0);
            unsafe {
                for pixel in 0..(64 * 64) {
                    std::ptr::copy_nonoverlapping(SEED_RGBA.as_ptr(), plane.add(pixel * 4), 4);
                }
            }
            full.copy_pixel_buffer_to_texture(
                &seed_buffer,
                source.texture(),
                "draw-source",
                64,
                64,
            )?;

            // The seed publishes SHADER_READ_ONLY_OPTIMAL — the very
            // layout a sampled binding wants — so the draw's input
            // barrier would have nothing to do and this test would end
            // by re-reading what setup established. GENERAL is a layout
            // the descriptor does not satisfy, which is the state a
            // storage-image producer upstream leaves behind.
            let mut recorder = full.create_command_recorder("draw_source_into_general")?;
            recorder.begin()?;
            recorder.record_image_barrier(
                source.texture(),
                crate::core::rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                crate::core::rhi::VulkanLayout::GENERAL,
                crate::vulkan::rhi::VulkanStage::ALL_COMMANDS,
                crate::vulkan::rhi::VulkanStage::ALL_COMMANDS,
                crate::vulkan::rhi::VulkanAccess::MEMORY_WRITE,
                crate::vulkan::rhi::VulkanAccess::MEMORY_READ,
            )?;
            recorder.submit_and_wait()?;
            full.resolve_texture_registration_by_surface_id("draw-source", None, 64, 64)?
                .update_layout(crate::core::rhi::VulkanLayout::GENERAL);
            Ok((source, target))
        })
        .expect("a seeded source and a colour target");

    let mut run = make_run_req("run-draw", &kernel_id, "draw-target");
    run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        surface_uuid: "draw-source".to_string(),
    }];
    run.extent_width = 64;
    run.extent_height = 64;
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunGraphicsDraw(run),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "run-draw");
            assert_eq!(
                ok.handle_id, kernel_id,
                "the run response echoes the kernel_id"
            );
            assert!(
                ok.timeline_value.is_none(),
                "run_graphics_draw responses carry no timeline"
            );
        }
        other => panic!("the draw failed: {other:?}"),
    }

    // Asserted before the readback, which transitions the image itself:
    // `offscreen_render` leaves every colour target in
    // COLOR_ATTACHMENT_OPTIMAL and tells no registration, so an
    // unpublished layout would leave the next consumer's barrier
    // naming an oldLayout the image has already left.
    let published = sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id("draw-target", None, 64, 64)?
                .current_layout())
        })
        .expect("the colour target still resolves");
    assert_eq!(
        published,
        streamlib_consumer_rhi::VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
        "the draw must publish the layout it left the colour target in"
    );

    let rendered = sandbox
        .escalate(|full| {
            let readback =
                full.create_texture_readback("draw-readback", 64, 64, TextureFormat::Rgba8Unorm)?;
            let ticket = readback.submit(
                held.1.texture(),
                crate::core::rhi::TextureSourceLayout::ColorAttachment,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the colour target reads back");
    for (pixel_index, pixel) in rendered.chunks_exact(4).enumerate() {
        assert_eq!(
            pixel, INVERTED_RGBA,
            "pixel {pixel_index} must be the inverted seed — the draw read \
                     `source_image`, by name, and painted the target it was given"
        );
    }

    // The bound input left GENERAL for the layout its descriptor
    // required, which is where the next consumer's barrier starts from.
    let source_layout = sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id("draw-source", None, 64, 64)?
                .current_layout())
        })
        .expect("the source still resolves");
    assert_eq!(
        source_layout,
        streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "a sampled binding is barriered out of GENERAL into the layout its descriptor \
                 requires"
    );
    drop(held);
}

/// The pixels a draw does not cover are the load op's, and this op
/// carries no clear colour of its own — so the handler's choice of
/// transparent black over `LOAD` is what they read.
///
/// The draw is scissored to the left half of a target seeded with a
/// sentinel no stage writes, so nothing but the load op ever touches the
/// right half. `LOAD` there reads an attachment the pass has just
/// transitioned from `UNDEFINED`, whose contents the spec stops defining
/// at that point — on this device the seeded sentinel survives it, which
/// is what makes the assertion discriminate.
#[test]
fn the_pixels_a_draw_does_not_cover_read_transparent_black() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("the_pixels_a_draw_does_not_cover: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("reg-scissored", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
    )
    .handle_id;

    let held = sandbox
        .escalate(|full| {
            let source = full.acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
            )?;
            let target = full.acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                    TextureUsages::RENDER_ATTACHMENT
                        | TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST,
                ),
            )?;
            full.register_texture("scissored-source", source.texture().clone());
            full.register_texture("scissored-target", target.texture().clone());

            for (texture, surface_id, seed) in [
                (source.texture(), "scissored-source", SEED_RGBA),
                (
                    target.texture(),
                    "scissored-target",
                    UNCOVERED_SENTINEL_RGBA,
                ),
            ] {
                let (_pool_id, seed_buffer) =
                    full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;
                let plane = seed_buffer.buffer_ref().plane_base_address(0);
                unsafe {
                    for pixel in 0..(64 * 64) {
                        std::ptr::copy_nonoverlapping(seed.as_ptr(), plane.add(pixel * 4), 4);
                    }
                }
                full.copy_pixel_buffer_to_texture(&seed_buffer, texture, surface_id, 64, 64)?;
            }
            Ok((source, target))
        })
        .expect("a seeded source and a seeded colour target");

    let mut run = make_run_req("run-scissored", &kernel_id, "scissored-target");
    run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        surface_uuid: "scissored-source".to_string(),
    }];
    run.extent_width = 64;
    run.extent_height = 64;
    run.scissor = Some(EscalateRequestRunGraphicsDrawScissor {
        x: 0,
        y: 0,
        width: 32,
        height: 64,
    });
    match handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunGraphicsDraw(run),
    )
    .expect("must produce a response")
    {
        EscalateResponse::Ok(_) => {}
        other => panic!("the scissored draw failed: {other:?}"),
    }

    let rendered = sandbox
        .escalate(|full| {
            let readback = full.create_texture_readback(
                "scissored-readback",
                64,
                64,
                TextureFormat::Rgba8Unorm,
            )?;
            let ticket = readback.submit(
                held.1.texture(),
                crate::core::rhi::TextureSourceLayout::ColorAttachment,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the colour target reads back");
    for (pixel_index, pixel) in rendered.chunks_exact(4).enumerate() {
        if pixel_index % 64 < 32 {
            assert_eq!(
                pixel, INVERTED_RGBA,
                "pixel {pixel_index} is inside the scissor and must be the inverted seed"
            );
        } else {
            assert_eq!(
                pixel, TRANSPARENT_BLACK_RGBA,
                "pixel {pixel_index} is outside the scissor, so nothing painted it — the \
                         pass must have cleared it rather than loaded contents its own transition \
                         from UNDEFINED had already discarded"
            );
        }
    }
    drop(held);
}

/// A draw whose binding and colour target are one texture is refused:
/// the pass discards a colour target's contents on entry, so the
/// binding would read pixels the draw has already thrown away.
#[test]
fn a_draw_binding_its_own_colour_target_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_binding_its_own_colour_target: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("reg-alias", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
    )
    .handle_id;
    let held = sandbox
        .escalate(|full| {
            let texture = full.acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT),
            )?;
            full.register_texture("alias-surface", texture.texture().clone());
            Ok(texture)
        })
        .expect("one texture to name twice");

    let mut run = make_run_req("run-alias", &kernel_id, "alias-surface");
    run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        surface_uuid: "alias-surface".to_string(),
    }];
    run.extent_width = 64;
    run.extent_height = 64;
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunGraphicsDraw(run),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("already thrown away"),
        "must say why the alias is refused, got: {message}"
    );
    drop(held);
}
