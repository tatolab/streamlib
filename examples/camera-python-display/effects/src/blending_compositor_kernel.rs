// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 4-layer alpha-over compositor kernel — sandboxed scenario content
//! (Linux only, engine-free).
//!
//! ## Why this lives in the example, not the engine
//!
//! This kernel and its shaders are sandboxed scenario content — the
//! cyberpunk N54 News PiP chrome is baked into the fragment shader,
//! so it doesn't belong in the engine. Real renderers use a render
//! graph that schedules barriers across passes and lets the CPU race
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
//!   transitions and the post-pass output barrier.
//! - [`GpuContextFullAccess::upload_pixel_buffer_as_texture`] — the 1×1
//!   placeholder upload (no raw `HostVulkanDevice`).
//!
//! None of these name a raw `HostVulkanDevice` or `vulkanalia` type — the
//! kernel is cdylib-safe end-to-end, so the image stays sound as a
//! separately-built `.slpkg`.
//!
//! ## Lifecycle
//!
//! Caller pre-allocates a ring of output `Texture`s (typically
//! `MAX_FRAMES_IN_FLIGHT = 2`), hands one to
//! [`SandboxedBlendingCompositor::dispatch`] per frame along with the four
//! layer textures + their current Vulkan layouts, and `dispatch` returns
//! once the GPU has signaled the underlying submits. After return, every
//! input texture and the output texture are left in
//! `SHADER_READ_ONLY_OPTIMAL`.

use std::sync::Mutex;

use streamlib_plugin_sdk::sdk::context::GpuContextFullAccess;
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    OffscreenColorTarget, OffscreenDraw, PixelFormat, PrimitiveTopology, RasterizationState,
    RhiCommandRecorder, ScissorRect, Texture, TextureFormat, VertexInputState, Viewport,
    VulkanAccess, VulkanGraphicsKernel, VulkanLayout, VulkanStage,
};

/// Fixed surface_id the 1×1 placeholder texture registers under in the
/// same-process texture cache. Hex-only UUIDv4 shape (`b1e0d` ≈ "blend",
/// `ff` octet marks the placeholder) so it never collides with a ring
/// slot.
const PLACEHOLDER_SURFACE_ID: &str = "00000000-0000-0000-0000-00000b1e0dff";

/// Push-constants layout — must match `blending_compositor.frag`'s
/// `layout(push_constant)` block byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlendingCompositorPushConstants {
    pub width: u32,
    pub height: u32,
    pub pip_width: u32,
    pub pip_height: u32,
    pub flags: u32,
    pub pip_slide_progress: f32,
}

/// `flags` bit positions for [`BlendingCompositorPushConstants`].
pub mod flag_bits {
    pub const HAS_VIDEO: u32 = 1 << 0;
    pub const HAS_LOWER_THIRD: u32 = 1 << 1;
    pub const HAS_WATERMARK: u32 = 1 << 2;
    pub const HAS_PIP: u32 = 1 << 3;
}

/// One layer input: texture + the layout it is currently in. The
/// compositor barriers from `current_layout` to
/// `SHADER_READ_ONLY_OPTIMAL` before the draw and leaves the texture
/// in that layout afterward.
#[derive(Clone, Copy)]
pub struct BlendingLayer<'a> {
    pub texture: &'a Texture,
    pub current_layout: VulkanLayout,
}

/// Render target for one composited frame: caller-owned ring slot.
///
/// Unlike [`BlendingLayer`], no `current_layout` is required —
/// [`VulkanGraphicsKernel::offscreen_render`] transitions the output
/// from `UNDEFINED` internally (content discard is permitted and the
/// full-screen triangle overwrites every pixel). The caller-side
/// post-pass barrier always lands the output in
/// `SHADER_READ_ONLY_OPTIMAL`.
#[derive(Clone, Copy)]
pub struct BlendingOutput<'a> {
    pub texture: &'a Texture,
}

/// Inputs for one compositor dispatch.
///
/// **Layer-size contract.** `video`, `lower_third`, and `watermark`
/// must match `output`'s dimensions exactly — the fragment shader
/// samples them at the same screen UV with no resampling, so a size
/// mismatch is rejected at dispatch time. `pip` may be any size; it is
/// sampled bilinearly inside the PiP rect via the kernel's default
/// linear sampler.
pub struct BlendingCompositorInputs<'a> {
    pub video: Option<BlendingLayer<'a>>,
    pub lower_third: Option<BlendingLayer<'a>>,
    pub watermark: Option<BlendingLayer<'a>>,
    pub pip: Option<BlendingLayer<'a>>,
    pub output: BlendingOutput<'a>,
    pub pip_slide_progress: f32,
}

/// 4-layer Porter-Duff "over" compositor with animated PiP frame chrome.
pub struct SandboxedBlendingCompositor {
    label: &'static str,
    kernel: VulkanGraphicsKernel,
    /// Reused across dispatches for the input pre-pass barrier (when
    /// any input is not already `SHADER_READ_ONLY_OPTIMAL`) and the
    /// post-pass output `COLOR_ATTACHMENT_OPTIMAL → SHADER_READ_ONLY_OPTIMAL`
    /// barrier that follows [`VulkanGraphicsKernel::offscreen_render`].
    recorder: Mutex<RhiCommandRecorder>,
    /// 1×1 transparent placeholder used for any unbound layer slot —
    /// graphics-kernel descriptor sets must be fully populated even
    /// when the corresponding `has_*` flag is false. Uploaded once at
    /// construction; left in `SHADER_READ_ONLY_OPTIMAL`.
    placeholder: Texture,
}

impl SandboxedBlendingCompositor {
    pub fn new(full: &GpuContextFullAccess) -> Result<Self> {
        let label = "blending_compositor";

        let vert = include_bytes!(concat!(env!("OUT_DIR"), "/blending_compositor.vert.spv"));
        let frag = include_bytes!(concat!(env!("OUT_DIR"), "/blending_compositor.frag.spv"));

        let stages = [GraphicsStage::vertex(vert), GraphicsStage::fragment(frag)];
        let bindings = [
            GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::sampled_texture(1, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::sampled_texture(2, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::sampled_texture(3, GraphicsShaderStageFlags::FRAGMENT),
        ];
        let descriptor = GraphicsKernelDescriptor {
            label,
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants {
                size: std::mem::size_of::<BlendingCompositorPushConstants>() as u32,
                stages: GraphicsShaderStageFlags::FRAGMENT,
            },
            pipeline_state: GraphicsPipelineState {
                topology: PrimitiveTopology::TriangleList,
                vertex_input: VertexInputState::None,
                rasterization: RasterizationState::default(),
                multisample: MultisampleState::default(),
                depth_stencil: DepthStencilState::Disabled,
                // Fragment shader does manual Porter-Duff alpha-over; no
                // hardware blend.
                color_blend: ColorBlendState::Disabled {
                    color_write_mask: ColorWriteMask::RGBA,
                },
                attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            // Synchronous dispatch (every call submits + waits) — one
            // descriptor set is enough.
            descriptor_sets_in_flight: 1,
        };
        let kernel = full.create_graphics_kernel(&descriptor)?;

        let recorder = full.create_command_recorder("blending_compositor_recorder")?;

        // 1×1 transparent placeholder — the descriptor set must bind a
        // real image for every sampled_texture binding even when the
        // corresponding `has_*` flag is off. The fragment shader gates the
        // actual sample via the flag, so the placeholder is never read; it
        // just keeps the descriptor legal.
        let placeholder = make_placeholder_texture(full)?;

        Ok(Self {
            label,
            kernel,
            recorder: Mutex::new(recorder),
            placeholder,
        })
    }

    /// Composite `inputs` into `inputs.output` and signal completion.
    ///
    /// Records (input barriers → offscreen render → output barrier),
    /// submits each via [`RhiCommandRecorder`] / [`VulkanGraphicsKernel::offscreen_render`],
    /// and waits before returning. After return, every input texture
    /// and the output texture are in `SHADER_READ_ONLY_OPTIMAL`. The
    /// input pre-pass is elided when every bound input is already in
    /// `SHADER_READ_ONLY_OPTIMAL` (the steady-state shape — typically
    /// 2 submits/dispatch, 3 only when an input layout shifts).
    pub fn dispatch(&self, inputs: BlendingCompositorInputs<'_>) -> Result<()> {
        let width = inputs.output.texture.width();
        let height = inputs.output.texture.height();

        // Layer-size contract — screen-aligned inputs must match the
        // output's dimensions exactly (PiP is sampler-rescaled, so it
        // is exempt).
        for (name, layer) in [
            ("video", inputs.video),
            ("lower_third", inputs.lower_third),
            ("watermark", inputs.watermark),
        ] {
            if let Some(layer) = layer {
                if layer.texture.width() != width || layer.texture.height() != height {
                    return Err(Error::GpuError(format!(
                        "{}: '{name}' layer is {}×{}, expected {width}×{height} (must match output)",
                        self.label,
                        layer.texture.width(),
                        layer.texture.height(),
                    )));
                }
            }
        }

        let (video, _vlayout) = self.layer_or_placeholder(inputs.video);
        let (lower_third, _lt_layout) = self.layer_or_placeholder(inputs.lower_third);
        let (watermark, _wm_layout) = self.layer_or_placeholder(inputs.watermark);
        let (pip, _pip_layout) = self.layer_or_placeholder(inputs.pip);
        let pip_dims = (pip.width(), pip.height());

        // Stage descriptor + push-constant writes onto the kernel
        // (single descriptor set since the kernel's ring depth is 1).
        self.kernel.set_sampled_texture(0, 0, video)?;
        self.kernel.set_sampled_texture(0, 1, lower_third)?;
        self.kernel.set_sampled_texture(0, 2, watermark)?;
        self.kernel.set_sampled_texture(0, 3, pip)?;

        let mut flags = 0u32;
        if inputs.video.is_some()       { flags |= flag_bits::HAS_VIDEO; }
        if inputs.lower_third.is_some() { flags |= flag_bits::HAS_LOWER_THIRD; }
        if inputs.watermark.is_some()   { flags |= flag_bits::HAS_WATERMARK; }
        if inputs.pip.is_some()         { flags |= flag_bits::HAS_PIP; }

        let push = BlendingCompositorPushConstants {
            width,
            height,
            pip_width: pip_dims.0,
            pip_height: pip_dims.1,
            flags,
            pip_slide_progress: inputs.pip_slide_progress.clamp(0.0, 1.0),
        };
        self.kernel.set_push_constants_value(0, &push)?;

        let mut recorder = self.recorder.lock().map_err(|e| {
            Error::GpuError(format!("{}: recorder mutex poisoned: {e}", self.label))
        })?;

        // Pre-pass: barrier every non-placeholder input whose current
        // layout isn't already SHADER_READ_ONLY_OPTIMAL. The
        // placeholder is born in SHADER_READ_ONLY_OPTIMAL (set by
        // `upload_pixel_buffer_as_texture`) and stays there forever, so
        // it never needs a barrier.
        let input_barriers: [Option<(&Texture, VulkanLayout)>; 4] = [
            inputs
                .video
                .filter(|l| l.current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL)
                .map(|l| (l.texture, l.current_layout)),
            inputs
                .lower_third
                .filter(|l| l.current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL)
                .map(|l| (l.texture, l.current_layout)),
            inputs
                .watermark
                .filter(|l| l.current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL)
                .map(|l| (l.texture, l.current_layout)),
            inputs
                .pip
                .filter(|l| l.current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL)
                .map(|l| (l.texture, l.current_layout)),
        ];
        if input_barriers.iter().any(Option::is_some) {
            recorder.begin()?;
            for slot in input_barriers.iter().flatten() {
                let (texture, from_layout) = *slot;
                // Tolerant src masks (ALL_COMMANDS + MEMORY_WRITE)
                // cover every upstream producer (camera compute, OpenGL
                // adapter glFinish, Skia/Vulkan adapters, future
                // encoders) without per-producer tuning.
                recorder.record_image_barrier(
                    texture,
                    from_layout,
                    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::FRAGMENT_SHADER,
                    VulkanAccess::MEMORY_WRITE,
                    VulkanAccess::SHADER_SAMPLED_READ,
                )?;
            }
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
        // SHADER_READ_ONLY_OPTIMAL so downstream consumers (display,
        // future encoders) can sample without re-barriering.
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

    fn layer_or_placeholder<'a>(
        &'a self,
        layer: Option<BlendingLayer<'a>>,
    ) -> (&'a Texture, VulkanLayout) {
        match layer {
            Some(BlendingLayer { texture, current_layout }) => (texture, current_layout),
            None => (&self.placeholder, VulkanLayout::SHADER_READ_ONLY_OPTIMAL),
        }
    }
}

/// Build the 1×1 transparent placeholder texture through the cdylib-safe
/// FullAccess pixel-buffer upload path. `upload_pixel_buffer_as_texture`
/// allocates a device texture, copies the host-visible buffer into it,
/// leaves it in `SHADER_READ_ONLY_OPTIMAL`, and registers it under
/// [`PLACEHOLDER_SURFACE_ID`]; we resolve that registration to obtain the
/// [`Texture`].
fn make_placeholder_texture(full: &GpuContextFullAccess) -> Result<Texture> {
    let (_pool_id, pixel_buffer) = full.acquire_pixel_buffer(1, 1, PixelFormat::Rgba32)?;
    let plane_size = pixel_buffer.plane_size(0);
    if plane_size < 4 {
        return Err(Error::GpuError(format!(
            "blending_compositor placeholder: pixel buffer plane 0 is {plane_size} bytes, need >= 4"
        )));
    }
    let base = pixel_buffer.plane_base_address(0);
    if base.is_null() {
        return Err(Error::GpuError(
            "blending_compositor placeholder: pixel buffer plane 0 base address is null".into(),
        ));
    }
    // SAFETY: `base` is a non-null host-visible mapping of >= 4 bytes
    // (checked above); write exactly 4 zero bytes (transparent RGBA).
    unsafe {
        std::ptr::write_bytes(base, 0, 4);
    }
    full.upload_pixel_buffer_as_texture(PLACEHOLDER_SURFACE_ID, &pixel_buffer, 1, 1)?;
    let registration = full.resolve_texture_registration_by_surface_id(
        PLACEHOLDER_SURFACE_ID,
        Some(VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0),
        1,
        1,
    )?;
    Ok(registration.texture().clone())
}
