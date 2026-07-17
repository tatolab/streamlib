// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Grayscale post-effect graphics kernel — sandboxed scenario content.
//!
//! ## Why this lives in the example, not the engine
//!
//! The grayscale effect is example content: the BT.601 luma weights are
//! baked into the fragment shader, so it doesn't belong in the engine.
//! It rides the engine-free `streamlib-plugin-sdk`'s cdylib-safe
//! [`VulkanGraphicsKernel::offscreen_render`] + [`RhiCommandRecorder`]
//! surfaces — the kernel + recorder are built through
//! [`GpuContextFullAccess::create_graphics_kernel`] /
//! [`GpuContextFullAccess::create_command_recorder`], never a raw host
//! device — so the cdylib links no engine facade and needs no allowlist
//! exception. Every queue-mutex / fence / Drop / barrier bug the engine
//! has fixed propagates here for free.
//!
//! ## Lifecycle
//!
//! The caller pre-allocates a ring of output `Texture`s, hands one to
//! [`SandboxedGrayscale::dispatch`] per frame along with the input texture
//! + its current Vulkan layout, and `dispatch` returns once the GPU has
//! signaled the underlying submits. After return, both input and output
//! textures are in `SHADER_READ_ONLY_OPTIMAL`, ready for the next
//! consumer to sample without re-barriering.

use std::sync::Mutex;

use streamlib_plugin_sdk::sdk::context::GpuContextFullAccess;
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    OffscreenColorTarget, OffscreenDraw, PrimitiveTopology, RasterizationState, RhiCommandRecorder,
    ScissorRect, Texture, TextureFormat, VertexInputState, Viewport, VulkanAccess,
    VulkanGraphicsKernel, VulkanLayout, VulkanStage,
};

/// Single input layer (the pre-effect texture) + its current Vulkan
/// layout. The kernel barriers from `current_layout` to
/// `SHADER_READ_ONLY_OPTIMAL` before the draw and leaves it there.
#[derive(Clone, Copy)]
pub struct GrayscaleInput<'a> {
    pub texture: &'a Texture,
    pub current_layout: VulkanLayout,
}

/// Render target for one grayscale dispatch. No `current_layout` is
/// required — [`VulkanGraphicsKernel::offscreen_render`] transitions the
/// output from `UNDEFINED` internally (content discard is permitted and
/// the full-screen triangle overwrites every pixel).
#[derive(Clone, Copy)]
pub struct GrayscaleOutput<'a> {
    pub texture: &'a Texture,
}

pub struct GrayscaleInputs<'a> {
    pub input: GrayscaleInput<'a>,
    pub output: GrayscaleOutput<'a>,
}

/// Grayscale post-effect graphics kernel.
pub struct SandboxedGrayscale {
    label: &'static str,
    kernel: VulkanGraphicsKernel,
    /// Reused across dispatches for the input pre-pass barrier (when the
    /// input is not already `SHADER_READ_ONLY_OPTIMAL`) and the post-pass
    /// output `COLOR_ATTACHMENT_OPTIMAL → SHADER_READ_ONLY_OPTIMAL`
    /// barrier that follows [`VulkanGraphicsKernel::offscreen_render`].
    recorder: Mutex<RhiCommandRecorder>,
}

impl SandboxedGrayscale {
    pub fn new(full: &GpuContextFullAccess) -> Result<Self> {
        let label = "grayscale";

        let vert = include_bytes!(concat!(env!("OUT_DIR"), "/grayscale.vert.spv"));
        let frag = include_bytes!(concat!(env!("OUT_DIR"), "/grayscale.frag.spv"));

        let stages = [GraphicsStage::vertex(vert), GraphicsStage::fragment(frag)];
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let descriptor = GraphicsKernelDescriptor {
            label,
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants::NONE,
            pipeline_state: GraphicsPipelineState {
                topology: PrimitiveTopology::TriangleList,
                vertex_input: VertexInputState::None,
                rasterization: RasterizationState::default(),
                multisample: MultisampleState::default(),
                depth_stencil: DepthStencilState::Disabled,
                color_blend: ColorBlendState::Disabled {
                    color_write_mask: ColorWriteMask::RGBA,
                },
                attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            descriptor_sets_in_flight: 1,
        };
        let kernel = full.create_graphics_kernel(&descriptor)?;

        let recorder = full.create_command_recorder("grayscale_recorder")?;

        Ok(Self {
            label,
            kernel,
            recorder: Mutex::new(recorder),
        })
    }

    /// Convert `inputs.input.texture` to grayscale into
    /// `inputs.output.texture`. Input must match the output 1:1 (the
    /// shader samples at the same screen UV).
    pub fn dispatch(&self, inputs: GrayscaleInputs<'_>) -> Result<()> {
        let width = inputs.output.texture.width();
        let height = inputs.output.texture.height();

        if inputs.input.texture.width() != width || inputs.input.texture.height() != height {
            return Err(Error::GpuError(format!(
                "{}: input is {}×{}, expected {width}×{height} (must match output)",
                self.label,
                inputs.input.texture.width(),
                inputs.input.texture.height(),
            )));
        }

        self.kernel
            .set_sampled_texture(0, 0, inputs.input.texture)?;

        let mut recorder = self.recorder.lock().map_err(|e| {
            Error::GpuError(format!("{}: recorder mutex poisoned: {e}", self.label))
        })?;

        // Pre-pass: barrier the input into SHADER_READ_ONLY_OPTIMAL when
        // it isn't already there. Tolerant src masks (ALL_COMMANDS +
        // MEMORY_WRITE) cover every upstream producer (camera compute,
        // adapters, future encoders) without per-producer tuning.
        if inputs.input.current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
            recorder.begin()?;
            recorder.record_image_barrier(
                inputs.input.texture,
                inputs.input.current_layout,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::FRAGMENT_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.submit_and_wait()?;
        }

        // Render pass. `offscreen_render` transitions output
        // `UNDEFINED → COLOR_ATTACHMENT_OPTIMAL` internally; the
        // full-screen triangle covers every pixel so `clear_color = None`
        // (LOAD) is sufficient. Output is left in
        // `COLOR_ATTACHMENT_OPTIMAL`.
        self.kernel.offscreen_render(
            0,
            &[OffscreenColorTarget {
                texture: inputs.output.texture,
                clear_color: None,
            }],
            (width, height),
            OffscreenDraw::Draw(DrawCall {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
                viewport: Some(Viewport::full(width, height)),
                scissor: Some(ScissorRect::full(width, height)),
            }),
        )?;

        // Post-pass: output COLOR_ATTACHMENT_OPTIMAL →
        // SHADER_READ_ONLY_OPTIMAL.
        recorder.begin()?;
        recorder.record_image_barrier(
            inputs.output.texture,
            VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanStage::COLOR_ATTACHMENT_OUTPUT,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::COLOR_ATTACHMENT_WRITE,
            VulkanAccess::SHADER_SAMPLED_READ,
        )?;
        recorder.submit_and_wait()?;

        Ok(())
    }
}
