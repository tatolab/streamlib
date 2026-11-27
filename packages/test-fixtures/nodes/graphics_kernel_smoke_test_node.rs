// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use streamlib::engine_internal::core::context::TexturePoolDescriptor;
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::nodes::{Input, computed};
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStage, GraphicsShaderStageFlags, GraphicsStage,
    MultisampleState, PrimitiveTopology, RasterizationState, TextureFormat, TextureUsages,
    VertexInputState,
};

use crate::result_written_to_its_file::the_result_written_to_its_file;

/// SPIR-V for the smoke-test vertex stage. Compiled by `build.rs`.
#[cfg(target_os = "linux")]
const SMOKE_VERT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/graphics_kernel_smoke_vert.spv"));

/// SPIR-V for the smoke-test fragment stage. Compiled by `build.rs`.
#[cfg(target_os = "linux")]
const SMOKE_FRAG_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/graphics_kernel_smoke_frag.spv"));

#[cfg(target_os = "linux")]
const SMOKE_BINDINGS: &[GraphicsBindingSpec] = &[];

#[cfg(target_os = "linux")]
const SMOKE_PUSH_CONSTANT_SIZE: u32 = std::mem::size_of::<u32>() as u32;
#[cfg(target_os = "linux")]
const SMOKE_SURFACE_SIZE: u32 = 64;

#[streamlib::sdk::node(
    execution = manual,
    input("output_path", description = "The file the node writes its result to.", value = String, default = String::new()),
)]
#[meta(
    description = "Graphics-kernel smoke test fixture — in its body, creates a graphics kernel via FullAccess, acquires a render-target Texture and runs a single offscreen_render() to assert the binding methods don't panic, then writes the result to the file its output_path names. Smoke-only; pixel correctness not asserted."
)]
pub fn graphics_kernel_smoke_test(output_path: Input<String>, ctx: &RuntimeContextFullAccess<'_>) {
    // The kernel is built and run once, in the body, with FullAccess.
    let outcome = run_graphics_kernel_smoke(ctx).map(|()| Vec::new());
    computed(move || {
        the_result_written_to_its_file("GraphicsKernelSmokeTest", &output_path.get(), &outcome)
    });
}

#[cfg(target_os = "linux")]
fn run_graphics_kernel_smoke(ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
    use streamlib::sdk::engine::host_rhi::{OffscreenColorTarget, OffscreenDraw};

    let gpu_limited = ctx.gpu_limited_access();

    let pool_descriptor = TexturePoolDescriptor::new(
        SMOKE_SURFACE_SIZE,
        SMOKE_SURFACE_SIZE,
        TextureFormat::Rgba8Unorm,
    )
    .with_usage(
        TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT,
    )
    .with_label("graphics_kernel_smoke_target");
    let texture_handle = gpu_limited
        .acquire_texture(&pool_descriptor)
        .map_err(|e| Error::Runtime(format!("acquire_texture: {e}")))?;

    // Manual-mode start() takes FullAccess directly.
    let full = ctx.gpu_full_access();
    let kernel = {
        let stages = [
            GraphicsStage {
                stage: GraphicsShaderStage::Vertex,
                spv: SMOKE_VERT_SPV,
                entry_point: "main",
            },
            GraphicsStage {
                stage: GraphicsShaderStage::Fragment,
                spv: SMOKE_FRAG_SPV,
                entry_point: "main",
            },
        ];
        let pipeline_state = GraphicsPipelineState {
            topology: PrimitiveTopology::TriangleList,
            vertex_input: VertexInputState::None,
            rasterization: RasterizationState::default(),
            multisample: MultisampleState::default(),
            depth_stencil: DepthStencilState::Disabled,
            color_blend: ColorBlendState::Disabled {
                color_write_mask: ColorWriteMask::RGBA,
            },
            attachment_formats: AttachmentFormats {
                color: vec![TextureFormat::Rgba8Unorm],
                depth: None,
            },
            dynamic_state: GraphicsDynamicState::ViewportScissor,
        };
        let descriptor = GraphicsKernelDescriptor {
            label: "graphics_kernel_smoke",
            stages: &stages,
            bindings: SMOKE_BINDINGS,
            push_constants: GraphicsPushConstants {
                size: SMOKE_PUSH_CONSTANT_SIZE,
                stages: GraphicsShaderStageFlags::VERTEX | GraphicsShaderStageFlags::FRAGMENT,
            },
            pipeline_state,
            descriptor_sets_in_flight: 1,
        };
        full.create_graphics_kernel(&descriptor)
            .map_err(|e| Error::Runtime(format!("create_graphics_kernel: {e}")))?
    };

    let variant: u32 = 0;
    kernel
        .set_push_constants_value(0, &variant)
        .map_err(|e| Error::Runtime(format!("set_push_constants_value: {e}")))?;

    let draw = OffscreenDraw::Draw(DrawCall {
        vertex_count: 3,
        instance_count: 1,
        first_vertex: 0,
        first_instance: 0,
        viewport: None,
        scissor: None,
    });
    kernel
        .offscreen_render(
            0,
            &[OffscreenColorTarget {
                texture: texture_handle.texture(),
                clear_color: Some([0.0, 0.0, 0.0, 1.0]),
            }],
            (SMOKE_SURFACE_SIZE, SMOKE_SURFACE_SIZE),
            draw,
        )
        .map_err(|e| Error::Runtime(format!("offscreen_render: {e}")))?;

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn run_graphics_kernel_smoke(_ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
    Err(Error::Runtime(
        "GraphicsKernelSmokeTest: graphics kernel dispatch is Linux-only today".into(),
    ))
}
