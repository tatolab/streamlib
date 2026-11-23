// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use streamlib::engine_internal::core::context::TexturePoolDescriptor;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::nodes::ManualNode;
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStage, GraphicsShaderStageFlags, GraphicsStage,
    MultisampleState, PrimitiveTopology, RasterizationState, TextureFormat, TextureUsages,
    VertexInputState,
};

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

/// The settings [`GraphicsKernelSmokeTest`] reads at setup.
#[streamlib::sdk::node_settings]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphicsKernelSmokeTestSettings {
    #[description = "The file the node writes its result to."]
    pub output_path: String,
}

#[streamlib::sdk::node(
    execution = manual,
    settings = crate::graphics_kernel_smoke_test_node::GraphicsKernelSmokeTestSettings,
)]
#[meta(
    description = "Graphics-kernel smoke test fixture — creates a graphics kernel via FullAccess, acquires a render-target Texture, runs a single offscreen_render() to assert the binding methods don't panic. Smoke-only; pixel correctness not asserted."
)]
pub struct GraphicsKernelSmokeTest {}

impl ManualNode for GraphicsKernelSmokeTest::Node {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.settings.output_path.clone();

        let outcome = run_graphics_kernel_smoke(ctx);

        let line = match outcome {
            Ok(()) => "OK".to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!("GraphicsKernelSmokeTest: write {output_path}: {e}"))
        })?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
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
