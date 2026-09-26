// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::super::acquisition::parse_texture_format;
use super::super::hex_encoded_wire_bytes::decode_hex;
use super::super::kernel_shader_stage_source::registered_shader_stage_source;
use super::super::surface_bound_kernel_binding::{
    DeclaredKernelBindingUnderPlanning, SuppliedKernelBindingUnderPlanning,
    bound_surface_layout_publish_pairs, descriptor_layout_transition_pairs,
    plan_supplied_surface_bound_kernel_bindings, publish_bound_surface_layouts_to_surface_share,
    reflected_kernel_binding_response, resolve_planned_surface_bound_kernel_bindings,
    transition_bound_kernel_inputs_into_descriptor_layouts,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateGraphicsBindingKind, EscalateRequestRegisterGraphicsKernel,
    EscalateRequestRegisterGraphicsKernelPipelineState,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateTopology, EscalateRequestRunGraphicsDraw,
    EscalateRequestRunGraphicsDrawDrawKind,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::{GlslCompilationTargetStage, SurfaceBoundKernelBindingKind};
use crate::host_rhi::HostTextureExt as _;

/// The RHI binding kind a graphics wire enum names.
pub(super) fn graphics_binding_kind_from_wire(
    kind: EscalateGraphicsBindingKind,
) -> crate::core::rhi::GraphicsBindingKind {
    use crate::core::rhi::GraphicsBindingKind;
    match kind {
        EscalateGraphicsBindingKind::SampledTexture => GraphicsBindingKind::SampledTexture,
        EscalateGraphicsBindingKind::StorageBuffer => GraphicsBindingKind::StorageBuffer,
        EscalateGraphicsBindingKind::StorageImage => GraphicsBindingKind::StorageImage,
        EscalateGraphicsBindingKind::UniformBuffer => GraphicsBindingKind::UniformBuffer,
    }
}

/// The wire enum for a graphics binding kind.
pub(super) fn graphics_binding_kind_to_wire(
    kind: crate::core::rhi::GraphicsBindingKind,
) -> EscalateGraphicsBindingKind {
    use crate::core::rhi::GraphicsBindingKind;
    match kind {
        GraphicsBindingKind::SampledTexture => EscalateGraphicsBindingKind::SampledTexture,
        GraphicsBindingKind::StorageBuffer => EscalateGraphicsBindingKind::StorageBuffer,
        GraphicsBindingKind::StorageImage => EscalateGraphicsBindingKind::StorageImage,
        GraphicsBindingKind::UniformBuffer => EscalateGraphicsBindingKind::UniformBuffer,
    }
}

/// Whether a graphics binding kind is one a draw can name a surface for.
pub(super) fn surface_bound_graphics_binding_kind(
    kind: crate::core::rhi::GraphicsBindingKind,
) -> Option<SurfaceBoundKernelBindingKind> {
    use crate::core::rhi::GraphicsBindingKind;
    match kind {
        GraphicsBindingKind::SampledTexture => Some(SurfaceBoundKernelBindingKind::SampledTexture),
        GraphicsBindingKind::StorageImage => Some(SurfaceBoundKernelBindingKind::StorageImage),
        GraphicsBindingKind::StorageBuffer | GraphicsBindingKind::UniformBuffer => None,
    }
}

/// Everything a `register_graphics_kernel` settles before it takes the device
/// gate: both stages compiled, the declaration read, the pipeline state
/// flattened.
pub(super) struct PreparedGraphicsKernelRegistration {
    label: String,
    vertex_spv: Arc<[u8]>,
    fragment_spv: Arc<[u8]>,
    vertex_entry_point: String,
    fragment_entry_point: String,
    declared_bindings: Vec<crate::core::rhi::GraphicsBindingDeclaration>,
    push_constants: crate::core::rhi::GraphicsPushConstants,
    pipeline_state: crate::core::rhi::GraphicsPipelineState,
    descriptor_sets_in_flight: u32,
}

/// Read a `register_graphics_kernel` request into what `GpuContext` builds a
/// kernel from, without touching the device.
///
/// Compilation is CPU work, and the escalate gate it would otherwise be holding
/// serializes every processor's device work — the same reason
/// [`RegisteredShaderStageSource::spirv`](crate::core::compiler::compiler_ops::subprocess_escalate::kernel_shader_stage_source::RegisteredShaderStageSource::spirv) takes the sandbox rather than a
/// `GpuContextFullAccess`.
pub(super) fn prepare_graphics_kernel_registration(
    sandbox: &GpuContextLimitedAccess,
    req: EscalateRequestRegisterGraphicsKernel,
) -> std::result::Result<PreparedGraphicsKernelRegistration, String> {
    use crate::core::rhi::{
        GraphicsBindingDeclaration, GraphicsPushConstants, GraphicsShaderStageFlags,
    };

    let vertex_source = registered_shader_stage_source(
        "vertex_",
        &req.vertex_source,
        &req.vertex_spv_hex,
        GlslCompilationTargetStage::Vertex,
        &req.vertex_entry_point,
    )?;
    let fragment_source = registered_shader_stage_source(
        "fragment_",
        &req.fragment_source,
        &req.fragment_spv_hex,
        GlslCompilationTargetStage::Fragment,
        &req.fragment_entry_point,
    )?;
    let vertex_spv = vertex_source.spirv(sandbox).map_err(|e| e.to_string())?;
    let fragment_spv = fragment_source.spirv(sandbox).map_err(|e| e.to_string())?;

    let mut declared_bindings = Vec::with_capacity(req.bindings.len());
    for wire in &req.bindings {
        declared_bindings.push(GraphicsBindingDeclaration {
            name: wire.name.clone(),
            kind: graphics_binding_kind_from_wire(wire.kind),
            stages: GraphicsShaderStageFlags::from_bits(wire.stages).ok_or_else(|| {
                format!(
                    "binding `{}` names stages {:#b}, which sets a bit no graphics stage owns \
                     (1 = vertex, 2 = fragment)",
                    wire.name, wire.stages
                )
            })?,
        });
    }

    let push_constants = GraphicsPushConstants {
        size: req.push_constant_size,
        stages: GraphicsShaderStageFlags::from_bits(req.push_constant_stages).ok_or_else(|| {
            format!(
                "push_constant_stages {:#b} sets a bit no graphics stage owns (1 = vertex, \
                 2 = fragment)",
                req.push_constant_stages
            )
        })?,
    };

    let pipeline_state = graphics_pipeline_state_from_wire(req.pipeline_state)
        .map_err(|e| format!("pipeline_state: {e}"))?;

    Ok(PreparedGraphicsKernelRegistration {
        label: req.label,
        vertex_spv,
        fragment_spv,
        vertex_entry_point: vertex_source.entry_point().to_string(),
        fragment_entry_point: fragment_source.entry_point().to_string(),
        declared_bindings,
        push_constants,
        pipeline_state,
        descriptor_sets_in_flight: req.descriptor_sets_in_flight,
    })
}

/// Build a graphics kernel for a subprocess customer, against `GpuContext`.
///
/// The graphics twin of [`handle_register_compute_kernel`](crate::core::compiler::compiler_ops::subprocess_escalate::compute::handle_register_compute_kernel): reflection over
/// both stages derives the binding shape and its names, the request's own
/// declaration is checked against it rather than replacing it, and
/// re-registering an identical kernel is a cache hit that answers with the same
/// `kernel_id`.
///
/// Each stage arrives as GLSL `*_source` the engine compiles, or as the
/// pre-compiled `*_spv_hex` escape hatch.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. A stage supplies neither `*_source` nor `*_spv_hex`, or both; its source
///    does not compile; or its hex doesn't decode.
/// 2. A binding's or the push-constant range's `stages` mask sets a bit no
///    graphics stage owns.
/// 3. `pipeline_state` names a shape a draw cannot run — MSAA, other than
///    exactly one colour attachment, `depth_stencil_enabled` or an
///    `attachment_depth_format` (the offscreen pass a draw runs through
///    attaches colour targets only), a `vertex_input_bindings` or
///    `vertex_input_attributes` entry (no escalate op mints a `VertexBuffer` to
///    fill one), an unknown colour format, or a write mask no channel owns.
/// 4. The blobs' `OpName` decorations were stripped — bindings resolve by name,
///    so an unnamed binding cannot be bound at all.
/// 5. The declaration disagrees with reflection on a name, a kind, or a stage.
/// 6. Push-constant size mismatch, or pipeline build failure.
pub(in super::super) fn handle_register_graphics_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterGraphicsKernel,
) -> EscalateResponse {
    use crate::core::rhi::{GraphicsShaderStage, GraphicsStage};

    let prepared = match prepare_graphics_kernel_registration(sandbox, req) {
        Ok(prepared) => prepared,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_graphics_kernel: {e}"),
            });
        }
    };

    let stages = [
        GraphicsStage {
            stage: GraphicsShaderStage::Vertex,
            spv: &prepared.vertex_spv,
            entry_point: &prepared.vertex_entry_point,
        },
        GraphicsStage {
            stage: GraphicsShaderStage::Fragment,
            spv: &prepared.fragment_spv,
            entry_point: &prepared.fragment_entry_point,
        },
    ];

    let registered = sandbox
        .escalate(|full| {
            full.create_or_reuse_graphics_kernel(
                &prepared.label,
                &stages,
                prepared.push_constants,
                &prepared.pipeline_state,
                prepared.descriptor_sets_in_flight,
                &prepared.declared_bindings,
            )
        })
        .and_then(|(kernel_id, kernel)| {
            // The caller draws by name and only the shaders know which kind
            // each name is, so the shape goes back with the id.
            let bindings = kernel
                .bindings()
                .iter()
                .map(|spec| {
                    reflected_kernel_binding_response(
                        &kernel_id,
                        spec.binding,
                        graphics_binding_kind_to_wire(spec.kind).wire_name(),
                        spec.name.as_deref(),
                    )
                })
                .collect::<crate::core::error::Result<Vec<_>>>()?;
            Ok((kernel_id, bindings))
        });

    match registered {
        Ok((kernel_id, bindings)) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            bindings: Some(bindings),
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_graphics_kernel failed: {e}"),
        }),
    }
}

/// Render one offscreen pass with a registered graphics kernel, its bindings
/// resolved by name.
///
/// The draw is synchronous on the host — `offscreen_render` submits and waits
/// on its own fence — so by the time this emits an `Ok`, the GPU work has
/// retired and the writes to the colour targets are visible to any later
/// submission on the same device.
///
/// Three shapes the wire carries have no host path and are refused rather than
/// silently dropped:
/// - `vertex_buffers` / `index_buffer` / an indexed draw. The setters take a
///   [`crate::core::rhi::VertexBuffer`] / [`crate::core::rhi::IndexBuffer`],
///   and no escalate op mints either — a helper can acquire a pixel buffer, a
///   texture or an image, none of which those setters accept.
/// - `depth_target_uuid`. The offscreen pass attaches colour targets only, so a
///   depth attachment would never be tested against.
///
/// Every binding error raises before anything is submitted, and names the
/// kernel's own bindings so the caller can see what it should have supplied.
pub(in super::super) fn handle_run_graphics_draw(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunGraphicsDraw,
) -> EscalateResponse {
    let unsupported = if !req.vertex_buffers.is_empty() {
        Some(format!(
            "vertex_buffers names {} buffer(s), and no escalate op mints a VertexBuffer — a \
             helper can acquire a pixel buffer, a texture or an image, and the vertex-buffer \
             setter takes none of them. Fabricate vertices from gl_VertexIndex instead",
            req.vertex_buffers.len()
        ))
    } else if req.index_buffer.is_some()
        || matches!(
            req.draw.kind,
            EscalateRequestRunGraphicsDrawDrawKind::DrawIndexed
        )
    {
        Some(
            "an indexed draw needs an IndexBuffer, and no escalate op mints one — a helper can \
             acquire a pixel buffer, a texture or an image, and the index-buffer setter takes \
             none of them"
                .to_string(),
        )
    } else if req.depth_target_uuid.is_some() {
        Some(
            "depth_target_uuid is set, and the offscreen pass this op drives attaches colour \
             targets only — the depth attachment would never be tested against"
                .to_string(),
        )
    } else if req.color_target_uuids.len() != 1 {
        Some(format!(
            "color_target_uuids names {} targets; the pipeline is built for exactly one colour \
             attachment",
            req.color_target_uuids.len()
        ))
    } else {
        None
    };
    if let Some(message) = unsupported {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_graphics_draw: {message}"),
        });
    }

    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_graphics_draw: push_constants_hex decode: {e}"),
            });
        }
    };

    let drawn = sandbox.escalate(|full| {
        let kernel = full.graphics_kernel_by_id(&req.kernel_id).ok_or_else(|| {
            crate::core::error::Error::GpuError(format!(
                "run_graphics_draw: no kernel registered under id {:?}",
                req.kernel_id
            ))
        })?;
        bind_and_render_graphics_kernel(full, &kernel, &req, &push_constants)
    });

    match drawn {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            // Echo the kernel_id back — the draw is sync host-side, so no
            // separate handle is allocated per draw.
            handle_id: req.kernel_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_graphics_draw failed: {e}"),
        }),
    }
}

/// Resolve every named binding onto the kernel's slots, render, and publish the
/// layout each colour target was left in.
///
/// The plan is total and every surface is resolved before the first `set_*`
/// call, so a refused draw never leaves the kernel holding a mix of this draw's
/// bindings and the last one's. The kernel's staged bindings are shared across
/// every caller of the cache; interleaving is prevented by the escalate gate,
/// which serializes the whole surrounding scope runtime-wide.
pub(super) fn bind_and_render_graphics_kernel(
    full: &crate::core::context::GpuContextFullAccess,
    kernel: &crate::vulkan::rhi::VulkanGraphicsKernel,
    req: &EscalateRequestRunGraphicsDraw,
    push_constants: &[u8],
) -> crate::core::error::Result<()> {
    use crate::core::error::Error;
    use crate::core::rhi::{DrawCall, ScissorRect, Viewport, VulkanLayout};
    use crate::vulkan::rhi::{OffscreenColorTarget, OffscreenDraw, VulkanStage};

    let declared_specs = kernel.bindings();
    let declared: Vec<DeclaredKernelBindingUnderPlanning<'_>> = declared_specs
        .iter()
        .map(|spec| DeclaredKernelBindingUnderPlanning {
            binding_slot: spec.binding,
            name: spec.name.as_deref(),
            kind_wire_name: graphics_binding_kind_to_wire(spec.kind).wire_name(),
            surface_bound_kind: surface_bound_graphics_binding_kind(spec.kind),
        })
        .collect();
    let supplied: Vec<SuppliedKernelBindingUnderPlanning<'_>> = req
        .bindings
        .iter()
        .map(|wire| SuppliedKernelBindingUnderPlanning {
            name: wire.name.as_str(),
            target_id: wire.surface_uuid.as_str(),
            kind_wire_name: wire.kind.wire_name(),
        })
        .collect();
    let planned = plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)?;

    // Held across the render, not consumed by the bind loop: a registration is
    // a refcount on the texture the descriptor set now points at, and dropping
    // the last one before the GPU has run frees the image out from under it.
    let bound_inputs = resolve_planned_surface_bound_kernel_bindings(full, planned)?;
    transition_bound_kernel_inputs_into_descriptor_layouts(
        full,
        "escalate_graphics_draw_input_layouts",
        VulkanStage::ALL_GRAPHICS,
        &descriptor_layout_transition_pairs(&bound_inputs),
    )?;

    let mut color_targets = Vec::with_capacity(req.color_target_uuids.len());
    for surface_id in &req.color_target_uuids {
        let registration = full
            .resolve_texture_registration_by_surface_id(surface_id, None, 0, 0)
            .map_err(|e| {
                Error::GpuError(format!(
                    "colour target {surface_id:?} is not something this graph can resolve to a \
                     device texture: {e}"
                ))
            })?;
        // A colour target enters the pass from UNDEFINED, which discards what
        // it held — so a binding reading the very image this draw renders into
        // reads discarded pixels. A target carrying no image is its own error,
        // raised where the attachment is built.
        let clashing_binding =
            registration
                .texture()
                .vulkan_inner()
                .image()
                .and_then(|target_image| {
                    bound_inputs.iter().find(|input| {
                        input.registration.texture().vulkan_inner().image() == Some(target_image)
                    })
                });
        if let Some(clashing) = clashing_binding {
            return Err(Error::GpuError(format!(
                "binding `{}` (surface {:?}) and colour target {surface_id:?} name one texture; \
                 the pass discards a colour target's contents on entry, so the binding would \
                 read pixels this draw has already thrown away",
                clashing.planned.name, clashing.planned.target_id
            )));
        }
        color_targets.push(registration);
    }

    for binding in &bound_inputs {
        let texture = binding.registration.texture();
        match binding.planned.kind {
            SurfaceBoundKernelBindingKind::SampledTexture => kernel.set_sampled_texture(
                req.frame_index,
                binding.planned.binding_slot,
                texture,
            )?,
            SurfaceBoundKernelBindingKind::StorageImage => {
                kernel.set_storage_image(req.frame_index, binding.planned.binding_slot, texture)?
            }
        }
    }

    // A kernel that declares push constants must be given them even when the
    // payload is empty, so `set_push_constants` produces the size mismatch
    // rather than the draw running against whatever the kernel's staged buffer
    // last held.
    if kernel.push_constant_size() > 0 || !push_constants.is_empty() {
        kernel.set_push_constants(req.frame_index, push_constants)?;
    }

    let draw = OffscreenDraw::Draw(DrawCall {
        vertex_count: req.draw.vertex_count,
        instance_count: req.draw.instance_count,
        first_vertex: req.draw.first_vertex,
        first_instance: req.draw.first_instance,
        viewport: req.viewport.as_ref().map(|v| Viewport {
            x: v.x,
            y: v.y,
            width: v.width,
            height: v.height,
            min_depth: v.min_depth,
            max_depth: v.max_depth,
        }),
        scissor: req.scissor.as_ref().map(|s| ScissorRect {
            x: s.x,
            y: s.y,
            width: s.width,
            height: s.height,
        }),
    });

    // `clear_color: None` would load an attachment the pass has just
    // transitioned from UNDEFINED, whose contents are undefined by then. The
    // op carries no clear colour of its own, so transparent black is what a
    // discarded target starts from.
    let attachments: Vec<OffscreenColorTarget<'_>> = color_targets
        .iter()
        .map(|registration| OffscreenColorTarget {
            texture: registration.texture(),
            clear_color: Some([0.0, 0.0, 0.0, 0.0]),
        })
        .collect();
    let rendered = kernel.offscreen_render(
        req.frame_index,
        &attachments,
        (req.extent_width, req.extent_height),
        draw,
    );
    drop(attachments);

    // `offscreen_render` transitions each colour target into
    // COLOR_ATTACHMENT_OPTIMAL and never tells its registration, so the record
    // would otherwise keep claiming the pre-draw layout and the next consumer's
    // barrier would name the wrong oldLayout. A refused draw transitioned
    // nothing, so only a rendered one publishes.
    if rendered.is_ok() {
        for registration in &color_targets {
            registration.update_layout(VulkanLayout::COLOR_ATTACHMENT_OPTIMAL);
        }
        let mut bound_surfaces = bound_surface_layout_publish_pairs(&bound_inputs);
        bound_surfaces.extend(
            req.color_target_uuids
                .iter()
                .cloned()
                .zip(color_targets.iter().cloned()),
        );
        publish_bound_surface_layouts_to_surface_share(full.surface_store(), &bound_surfaces);
    }
    drop(color_targets);
    drop(bound_inputs);
    rendered
}

/// One arm-for-arm mapping from a wire blend-factor enum to the RHI's.
///
/// A macro rather than a function per enum: the wire carries four separate
/// factor enums with identical arms, and four hand-written copies of the same
/// fifteen-arm match is four things to keep in step.
macro_rules! blend_factor_from_wire {
    ($enum:ident, $value:expr) => {{
        use $enum as W;

        use crate::core::rhi::BlendFactor;
        match $value {
            W::Zero => BlendFactor::Zero,
            W::One => BlendFactor::One,
            W::SrcColor => BlendFactor::SrcColor,
            W::OneMinusSrcColor => BlendFactor::OneMinusSrcColor,
            W::DstColor => BlendFactor::DstColor,
            W::OneMinusDstColor => BlendFactor::OneMinusDstColor,
            W::SrcAlpha => BlendFactor::SrcAlpha,
            W::OneMinusSrcAlpha => BlendFactor::OneMinusSrcAlpha,
            W::DstAlpha => BlendFactor::DstAlpha,
            W::OneMinusDstAlpha => BlendFactor::OneMinusDstAlpha,
            W::ConstantColor => BlendFactor::ConstantColor,
            W::OneMinusConstantColor => BlendFactor::OneMinusConstantColor,
            W::ConstantAlpha => BlendFactor::ConstantAlpha,
            W::OneMinusConstantAlpha => BlendFactor::OneMinusConstantAlpha,
            W::SrcAlphaSaturate => BlendFactor::SrcAlphaSaturate,
        }
    }};
}

/// One arm-for-arm mapping from a wire blend-op enum to the RHI's, for the same
/// reason [`blend_factor_from_wire`] is a macro.
macro_rules! blend_op_from_wire {
    ($enum:ident, $value:expr) => {{
        use $enum as W;

        use crate::core::rhi::BlendOp;
        match $value {
            W::Add => BlendOp::Add,
            W::Subtract => BlendOp::Subtract,
            W::ReverseSubtract => BlendOp::ReverseSubtract,
            W::Min => BlendOp::Min,
            W::Max => BlendOp::Max,
        }
    }};
}

/// Flatten the wire's one-level pipeline state into the RHI's nested one.
///
/// The wire is flat because JSON has no sum types: every field is present and
/// the flags decide which ones mean anything. The RHI's sum types are what the
/// pipeline is actually built from, so the two shapes meet here.
///
/// Refuses what a draw over this op has no path for — MSAA beyond one sample,
/// other than exactly one colour attachment, either half of a depth attachment,
/// either half of a vertex input, a colour format the texture vocabulary doesn't
/// name, and a write mask naming a bit no channel owns.
pub(super) fn graphics_pipeline_state_from_wire(
    p: EscalateRequestRegisterGraphicsKernelPipelineState,
) -> std::result::Result<crate::core::rhi::GraphicsPipelineState, String> {
    use crate::core::rhi::{
        AttachmentFormats, ColorBlendAttachment, ColorBlendState, ColorWriteMask, CullMode,
        DepthStencilState, FrontFace, GraphicsDynamicState, GraphicsPipelineState,
        MultisampleState, PolygonMode, PrimitiveTopology, RasterizationState, VertexInputState,
    };

    if p.multisample_samples != 1 {
        return Err(format!(
            "multisample_samples is {}; the graphics kernel builds single-sampled pipelines only",
            p.multisample_samples
        ));
    }
    if p.attachment_color_formats.len() != 1 {
        return Err(format!(
            "attachment_color_formats names {} formats; the graphics kernel targets exactly one \
             colour attachment",
            p.attachment_color_formats.len()
        ));
    }
    // The offscreen pass a draw runs through attaches colour targets only, so a
    // pipeline declaring a depth attachment mismatches the rendering info at
    // every draw. `run_graphics_draw` refuses `depth_target_uuid` for the same
    // reason; refusing only there would let the mismatch be built at register
    // time and surface as a driver error a draw away from its cause.
    if p.depth_stencil_enabled {
        return Err(
            "depth_stencil_enabled is set, and the offscreen pass a draw runs through attaches \
             colour targets only — a depth-testing pipeline has no attachment to test against"
                .to_string(),
        );
    }
    if p.attachment_depth_format.is_some() {
        return Err(
            "attachment_depth_format names a depth attachment, and the offscreen pass a draw runs \
             through attaches colour targets only — the pipeline's formats would disagree with \
             the pass at every draw"
                .to_string(),
        );
    }

    // A pipeline pulling from a vertex binding could register and then never
    // draw: `run_graphics_draw` refuses `vertex_buffers` because no escalate op
    // mints a `VertexBuffer`, and the kernel refuses a declared binding whose
    // buffer was never set at every draw. Refused here, the caller meets the
    // reason where the shape is asked for rather than a submission away from it.
    if !p.vertex_input_bindings.is_empty() {
        return Err(format!(
            "vertex_input_bindings names {} binding(s), and no escalate op mints a VertexBuffer to \
             fill one — a helper can acquire a pixel buffer, a texture or an image, and the \
             vertex-buffer setter takes none of them, so this pipeline would register and then be \
             refused at every draw. Fabricate vertices from gl_VertexIndex instead",
            p.vertex_input_bindings.len()
        ));
    }
    if !p.vertex_input_attributes.is_empty() {
        return Err(format!(
            "vertex_input_attributes names {} attribute(s), and an attribute is pulled from a \
             vertex binding no escalate op can mint a buffer for. Fabricate vertices from \
             gl_VertexIndex instead",
            p.vertex_input_attributes.len()
        ));
    }

    let topology = match p.topology {
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::PointList => {
            PrimitiveTopology::PointList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::LineList => {
            PrimitiveTopology::LineList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::LineStrip => {
            PrimitiveTopology::LineStrip
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleList => {
            PrimitiveTopology::TriangleList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleStrip => {
            PrimitiveTopology::TriangleStrip
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleFan => {
            PrimitiveTopology::TriangleFan
        }
    };

    let rasterization = RasterizationState {
        polygon_mode: match p.rasterization_polygon_mode {
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Fill => {
                PolygonMode::Fill
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Line => {
                PolygonMode::Line
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Point => {
                PolygonMode::Point
            }
        },
        cull_mode: match p.rasterization_cull_mode {
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::None => {
                CullMode::None
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Front => {
                CullMode::Front
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Back => {
                CullMode::Back
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::FrontAndBack => {
                CullMode::FrontAndBack
            }
        },
        front_face: match p.rasterization_front_face {
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::CounterClockwise => {
                FrontFace::CounterClockwise
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::Clockwise => {
                FrontFace::Clockwise
            }
        },
        line_width: p.rasterization_line_width,
    };

    let color_write_mask = ColorWriteMask::from_bits(p.color_write_mask).ok_or_else(|| {
        format!(
            "color_write_mask {:#b} sets a bit no colour channel owns (1 = R, 2 = G, 4 = B, \
             8 = A)",
            p.color_write_mask
        )
    })?;
    let color_blend = if p.color_blend_enabled {
        ColorBlendState::Enabled(ColorBlendAttachment {
            src_color_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
                p.color_blend_src_color_factor
            ),
            dst_color_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
                p.color_blend_dst_color_factor
            ),
            color_blend_op: blend_op_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
                p.color_blend_color_op
            ),
            src_alpha_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
                p.color_blend_src_alpha_factor
            ),
            dst_alpha_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
                p.color_blend_dst_alpha_factor
            ),
            alpha_blend_op: blend_op_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
                p.color_blend_alpha_op
            ),
            color_write_mask,
        })
    } else {
        ColorBlendState::Disabled { color_write_mask }
    };

    let mut color = Vec::with_capacity(p.attachment_color_formats.len());
    for format in &p.attachment_color_formats {
        color.push(
            parse_texture_format(format).map_err(|e| format!("attachment_color_formats: {e}"))?,
        );
    }
    let attachment_formats = AttachmentFormats { color, depth: None };

    let dynamic_state = match p.dynamic_state {
        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::None => {
            GraphicsDynamicState::None
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::ViewportScissor => {
            GraphicsDynamicState::ViewportScissor
        }
    };

    Ok(GraphicsPipelineState {
        topology,
        vertex_input: VertexInputState::None,
        rasterization,
        multisample: MultisampleState {
            samples: p.multisample_samples,
        },
        depth_stencil: DepthStencilState::Disabled,
        color_blend,
        attachment_formats,
        dynamic_state,
    })
}
