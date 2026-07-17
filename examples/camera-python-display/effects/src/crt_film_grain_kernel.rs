// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 80s Blade Runner CRT + film-grain post-effect kernel — sandboxed
//! scenario content (Linux only, engine-free).
//!
//! ## Why this lives in the example, not the engine
//!
//! This kernel and its shaders are sandboxed scenario content — the
//! Blade Runner CRT vibe is baked into the fragment shader, so it
//! doesn't belong in the engine. Real renderers use a render graph
//! that schedules barriers across passes and lets the CPU race
//! ahead 1–2 frames; the synchronous-blocking shape of a per-frame
//! "dispatch" helper stalls the CPU every frame, which is why
//! production engines (UE5, Bevy, Granite, wgpu) deliberately don't
//! ship one.
//!
//! ## Engine surfaces this rides
//!
//! Everything goes through the engine-free `streamlib-plugin-sdk`'s
//! cdylib-safe FullAccess / method primitives:
//! - [`GpuContextFullAccess::create_graphics_kernel`] — the fullscreen-
//!   fragment-effect pipeline, built once at setup.
//! - [`VulkanGraphicsKernel::offscreen_render`] — opens the dynamic-
//!   rendering pass, transitions output `UNDEFINED →
//!   COLOR_ATTACHMENT_OPTIMAL`, records the draw, submits, waits.
//! - [`RhiCommandRecorder`] (`record_image_barrier`) — the input-side
//!   transition and the post-pass output barrier.
//!
//! None of these name a raw `HostVulkanDevice` or `vulkanalia` type — the
//! kernel is cdylib-safe end-to-end, so the image stays sound as a
//! separately-built `.slpkg`.
//!
//! ## Lifecycle
//!
//! Caller pre-allocates a ring of output `Texture`s (mirrors
//! `BlendingCompositor`'s `OUTPUT_RING_DEPTH = 2`), hands one to
//! [`SandboxedCrtFilmGrain::dispatch`] per frame along with the input
//! texture + its current Vulkan layout, and `dispatch` returns once
//! the GPU has signaled the underlying submits. After return, both
//! input and output textures are in `SHADER_READ_ONLY_OPTIMAL`.

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

/// Push-constants layout — must match `crt_film_grain.frag`'s
/// `layout(push_constant)` block byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CrtFilmGrainPushConstants {
    pub width: u32,
    pub height: u32,
    pub time: f32,
    pub crt_curve: f32,
    pub scanline_intensity: f32,
    pub chromatic_aberration: f32,
    pub grain_intensity: f32,
    pub grain_speed: f32,
    pub vignette_intensity: f32,
    pub brightness: f32,
}

/// Single input layer (the pre-effect texture) + its current Vulkan
/// layout. The kernel barriers from `current_layout` to
/// `SHADER_READ_ONLY_OPTIMAL` before the draw and leaves it there
/// afterward.
#[derive(Clone, Copy)]
pub struct CrtFilmGrainInput<'a> {
    pub texture: &'a Texture,
    pub current_layout: VulkanLayout,
}

/// Render target for one CRT/film-grain dispatch.
///
/// Unlike [`CrtFilmGrainInput`], no `current_layout` is required —
/// [`VulkanGraphicsKernel::offscreen_render`] transitions the output
/// from `UNDEFINED` internally (content discard is permitted and the
/// full-screen triangle overwrites every pixel). The caller-side
/// post-pass barrier always lands the output in
/// `SHADER_READ_ONLY_OPTIMAL`.
#[derive(Clone, Copy)]
pub struct CrtFilmGrainOutput<'a> {
    pub texture: &'a Texture,
}

pub struct CrtFilmGrainInputs<'a> {
    pub input: CrtFilmGrainInput<'a>,
    pub output: CrtFilmGrainOutput<'a>,
    pub time_seconds: f32,
    pub crt_curve: f32,
    pub scanline_intensity: f32,
    pub chromatic_aberration: f32,
    pub grain_intensity: f32,
    pub grain_speed: f32,
    pub vignette_intensity: f32,
    pub brightness: f32,
}

/// CRT + film-grain post-effect graphics kernel.
pub struct SandboxedCrtFilmGrain {
    label: &'static str,
    kernel: VulkanGraphicsKernel,
    /// Reused across dispatches for the input pre-pass barrier (when
    /// the input is not already `SHADER_READ_ONLY_OPTIMAL`) and the
    /// post-pass output `COLOR_ATTACHMENT_OPTIMAL → SHADER_READ_ONLY_OPTIMAL`
    /// barrier that follows [`VulkanGraphicsKernel::offscreen_render`].
    recorder: Mutex<RhiCommandRecorder>,
}

impl SandboxedCrtFilmGrain {
    pub fn new(full: &GpuContextFullAccess) -> Result<Self> {
        let label = "crt_film_grain";

        let vert = include_bytes!(concat!(env!("OUT_DIR"), "/crt_film_grain.vert.spv"));
        let frag = include_bytes!(concat!(env!("OUT_DIR"), "/crt_film_grain.frag.spv"));

        let stages = [GraphicsStage::vertex(vert), GraphicsStage::fragment(frag)];
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let descriptor = GraphicsKernelDescriptor {
            label,
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants {
                size: std::mem::size_of::<CrtFilmGrainPushConstants>() as u32,
                stages: GraphicsShaderStageFlags::FRAGMENT,
            },
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

        let recorder = full.create_command_recorder("crt_film_grain_recorder")?;

        Ok(Self {
            label,
            kernel,
            recorder: Mutex::new(recorder),
        })
    }

    /// Apply the CRT/film-grain effect from `inputs.input.texture`
    /// into `inputs.output.texture`. Output dimensions drive the
    /// viewport/scissor; input must match the output 1:1 (the shader
    /// samples at the same screen UV).
    pub fn dispatch(&self, inputs: CrtFilmGrainInputs<'_>) -> Result<()> {
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

        self.kernel.set_sampled_texture(0, 0, inputs.input.texture)?;

        let push = CrtFilmGrainPushConstants {
            width,
            height,
            time: inputs.time_seconds,
            crt_curve: inputs.crt_curve,
            scanline_intensity: inputs.scanline_intensity,
            chromatic_aberration: inputs.chromatic_aberration,
            grain_intensity: inputs.grain_intensity,
            grain_speed: inputs.grain_speed,
            vignette_intensity: inputs.vignette_intensity,
            brightness: inputs.brightness,
        };
        self.kernel.set_push_constants_value(0, &push)?;

        let mut recorder = self.recorder.lock().map_err(|e| {
            Error::GpuError(format!("{}: recorder mutex poisoned: {e}", self.label))
        })?;

        // Pre-pass: barrier the input into SHADER_READ_ONLY_OPTIMAL
        // when it isn't already there. Tolerant src masks
        // (ALL_COMMANDS + MEMORY_WRITE) cover every upstream producer
        // (camera compute, OpenGL adapter glFinish, Skia/Vulkan
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
        // full-screen triangle covers every pixel so `clear_color =
        // None` (LOAD) is sufficient. Output is left in
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
