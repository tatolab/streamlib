// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration-test fixture: a dlopen'd processor that exercises a
//! handful of the `VulkanGraphicsKernelMethodsVTable` slots end-to-end
//! from cdylib code.
//!
//! Smoke-only — narrower scope than the compute-kernel CPU-reference
//! test. Validates that the vtable round-trips for kernel construction,
//! a couple of binding-method setters, push-constant staging, and an
//! `offscreen_render()` dispatch complete without panicking or
//! returning an error code. Pixel correctness is not asserted.
//!
//! Lifecycle:
//!   1. `setup()` — nothing.
//!   2. `start()` —
//!      a. Construct a [`GraphicsKernelDescriptor`] for the embedded
//!         vert+frag SPIR-V (centered triangle, push-constant
//!         variant gate, single Rgba8Unorm color attachment) and
//!         create the kernel via
//!         `gpu_full_access().create_graphics_kernel(...)`. Exercises
//!         the FullAccess vtable's `create_graphics_kernel` slot.
//!      b. Acquire a render-target `Texture` (Rgba8Unorm,
//!         COPY_DST | TEXTURE_BINDING | RENDER_ATTACHMENT) via
//!         `gpu_limited_access().acquire_texture(...)`.
//!      c. Stage push constants via
//!         `kernel.set_push_constants_value(0, &variant)` — exercises
//!         the `set_push_constants` vtable slot.
//!      d. Drive `kernel.offscreen_render(...)` against the acquired
//!         texture with a single CLEAR-load color target — exercises
//!         the `offscreen_render` vtable slot end-to-end (including
//!         the parallel-array color-target marshaling + the
//!         `OffscreenDrawRepr` tagged-union encoding).
//!      e. Write `OK` or `ERR:<message>` to the configured
//!         `output_path` so the integration test can assert the
//!         round-trip succeeded.
//!   3. `teardown()` — nothing; the kernel + texture drop naturally.
//!
//! What this locks: a regression that breaks any of
//! `create_graphics_kernel`, `acquire_texture`, `set_push_constants`,
//! or `offscreen_render` at the cdylib boundary surfaces here as
//! either a missing output file (cdylib panicked at the FFI
//! boundary) or `ERR:<message>` in the file.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStage, GraphicsShaderStageFlags, GraphicsStage,
    MultisampleState, PrimitiveTopology, RasterizationState, TextureFormat, TextureUsages,
    VertexInputState,
};
use streamlib::engine_internal::core::context::TexturePoolDescriptor;

/// SPIR-V for the smoke-test vertex stage. Compiled by `build.rs`.
const SMOKE_VERT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/graphics_kernel_smoke_vert.spv"));

/// SPIR-V for the smoke-test fragment stage. Compiled by `build.rs`.
const SMOKE_FRAG_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/graphics_kernel_smoke_frag.spv"));

const SMOKE_BINDINGS: &[GraphicsBindingSpec] = &[];

const SMOKE_PUSH_CONSTANT_SIZE: u32 = std::mem::size_of::<u32>() as u32;
const SMOKE_SURFACE_SIZE: u32 = 64;

#[streamlib::sdk::processor("GraphicsKernelSmokeTestProcessor")]
pub struct GraphicsKernelSmokeTest {}

impl ManualProcessor for GraphicsKernelSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        let outcome = run_graphics_kernel_smoke(ctx);

        let line = match outcome {
            Ok(()) => "OK".to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!(
                "GraphicsKernelSmokeTest: write {output_path}: {e}"
            ))
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

    let pool_descriptor = TexturePoolDescriptor {
        width: SMOKE_SURFACE_SIZE,
        height: SMOKE_SURFACE_SIZE,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::COPY_DST
            | TextureUsages::TEXTURE_BINDING
            | TextureUsages::RENDER_ATTACHMENT,
        label: Some("graphics_kernel_smoke_target"),
    };
    let texture_handle = gpu_limited
        .acquire_texture(&pool_descriptor)
        .map_err(|e| Error::Runtime(format!("acquire_texture: {e}")))?;

    // Manual-mode start() takes FullAccess directly; the engine
    // wraps cdylib lifecycle dispatch in `with_cdylib_scope` (#1075),
    // so `ctx.gpu_full_access()` is `ScopeToken`-flavored and
    // dispatches through the FullAccess vtable transparently.
    // Same coverage as the pre-#1075 escalate path; the wrap is the
    // engine-side replacement for the explicit `.escalate(|full|...)`.
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
                stages: GraphicsShaderStageFlags::VERTEX
                    | GraphicsShaderStageFlags::FRAGMENT,
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
