// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 4-layer alpha-over compositor kernel — sandboxed scenario content.
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
//! - [`VulkanGraphicsKernel::offscreen_render`] — the cdylib-safe
//!   Texture-typed render-scope helper. Opens the dynamic-rendering
//!   pass internally, transitions output `UNDEFINED →
//!   COLOR_ATTACHMENT_OPTIMAL`, records `cmd_bind_and_draw`, submits
//!   through the host's queue mutex, waits. Output is left in
//!   `COLOR_ATTACHMENT_OPTIMAL`.
//! - [`RhiCommandRecorder`] + [`record_image_barrier`] — for the
//!   input-side transitions (when an input isn't already
//!   `SHADER_READ_ONLY_OPTIMAL`) and the post-pass `COLOR_ATTACHMENT_OPTIMAL
//!   → SHADER_READ_ONLY_OPTIMAL` transition on the output.
//!
//! Neither surface exposes raw `vulkanalia` types — the kernel is
//! cdylib-safe end-to-end. Every queue-mutex / fence / Drop / barrier
//! bug the engine has fixed propagates here for free.
//!
//! ## Lifecycle
//!
//! Caller pre-allocates a ring of output `Texture`s (typically
//! `MAX_FRAMES_IN_FLIGHT = 2`), hands one to [`SandboxedBlendingCompositor::dispatch`]
//! per frame along with the four layer textures + their current
//! Vulkan layouts, and `dispatch` returns once the GPU has signaled
//! the underlying submits. After return, every input texture and the
//! output texture are left in `SHADER_READ_ONLY_OPTIMAL`, ready for
//! the next consumer to sample without re-barriering.
//!
//! [`record_image_barrier`]: streamlib::sdk::engine::host_rhi::RhiCommandRecorder::record_image_barrier

use std::sync::{Arc, Mutex};
use streamlib::sdk::engine::HostTextureExt;

use streamlib::sdk::rhi::{
    AttachmentFormats,
    ColorBlendState,
    ColorWriteMask,
    DepthStencilState,
    DrawCall,
    GraphicsBindingSpec,
    GraphicsDynamicState,
    GraphicsKernelDescriptor,
    GraphicsPipelineState,
    GraphicsPushConstants,
    GraphicsShaderStageFlags,
    GraphicsStage,
    MultisampleState,
    PrimitiveTopology,
    RasterizationState,
    ScissorRect,
    Texture,
    TextureDescriptor,
    TextureFormat,
    TextureUsages,
    VertexInputState,
    Viewport,
    VulkanLayout,
};
use streamlib::sdk::error::{Result, Error};
use streamlib::sdk::engine::host_rhi::{
    HostVulkanDevice,
    HostVulkanBuffer,
    OffscreenColorTarget,
    OffscreenDraw,
    RhiCommandRecorder,
    VulkanAccess,
    VulkanGraphicsKernel,
    VulkanStage,
};

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
    /// 1×1 transparent BGRA texture used for any unbound layer slot —
    /// graphics-kernel descriptor sets must be fully populated even
    /// when the corresponding `has_*` flag is false. Pre-uploaded once
    /// at construction; ends in `SHADER_READ_ONLY_OPTIMAL`.
    placeholder: Texture,
}

impl SandboxedBlendingCompositor {
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>) -> Result<Self> {
        let label = "blending_compositor";

        let vert =
            include_bytes!(concat!(env!("OUT_DIR"), "/blending_compositor.vert.spv"));
        let frag =
            include_bytes!(concat!(env!("OUT_DIR"), "/blending_compositor.frag.spv"));

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
        let kernel = VulkanGraphicsKernel::new(vulkan_device, &descriptor)?;

        let recorder = RhiCommandRecorder::new(vulkan_device, "blending_compositor_recorder")?;

        // 1×1 transparent BGRA placeholder — the descriptor set must
        // bind a real image for every sampled_texture binding even
        // when the corresponding `has_*` flag is off. The fragment
        // shader gates the actual sample via the flag, so the
        // placeholder is never read; it just keeps the descriptor
        // legal.
        let placeholder = make_placeholder_texture(vulkan_device)?;

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
        // `upload_buffer_to_image`) and stays there forever, so it
        // never needs a barrier.
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

fn make_placeholder_texture(vulkan_device: &Arc<HostVulkanDevice>) -> Result<Texture> {
    let desc = TextureDescriptor::new(1, 1, TextureFormat::Bgra8Unorm).with_usage(
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
    );
    // Local (non-DMA-BUF) texture is fine for an internal placeholder —
    // it never crosses a process boundary.
    let host_tex = vulkan_device.create_texture_local(&desc)?;
    let image = host_tex.image().ok_or_else(|| {
        Error::GpuError("placeholder texture has no VkImage".into())
    })?;

    // Upload zeros (transparent BGRA); `upload_buffer_to_image` leaves
    // the image in SHADER_READ_ONLY_OPTIMAL.
    let staging = HostVulkanBuffer::new(vulkan_device, 4)?;
    unsafe {
        std::ptr::write_bytes(staging.mapped_ptr(), 0, 4);
        vulkan_device.upload_buffer_to_image(staging.buffer(), image, 1, 1)?;
    }

    Ok(Texture::from_vulkan(host_tex))
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::rhi::{TextureReadbackDescriptor, TextureSourceLayout};
    use streamlib::sdk::engine::host_rhi::VulkanTextureReadback;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    /// Allocate a render-target-capable texture for compositor input or
    /// output use. Local (non-DMA-BUF) — these are unit-test fixtures
    /// and never cross a process boundary.
    fn make_render_texture(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
    ) -> Texture {
        let desc = TextureDescriptor::new(width, height, TextureFormat::Bgra8Unorm).with_usage(
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
        );
        let host_tex = device.create_texture_local(&desc).expect("texture");
        Texture::from_vulkan(host_tex)
    }

    /// Fill a texture with a single BGRA color via host-visible staging
    /// buffer + cmd_copy_buffer_to_image. Leaves the image in
    /// SHADER_READ_ONLY_OPTIMAL.
    fn fill_texture_solid(
        device: &Arc<HostVulkanDevice>,
        texture: &Texture,
        b: u8,
        g: u8,
        r: u8,
        a: u8,
    ) {
        let w = texture.width();
        let h = texture.height();
        let staging = HostVulkanBuffer::new(device, (w as u64) * (h as u64) * (4 as u64))
            .expect("staging");
        let pixel = (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24);
        unsafe {
            let ptr = staging.mapped_ptr() as *mut u32;
            for i in 0..(w * h) as usize {
                *ptr.add(i) = pixel;
            }
        }
        let image = texture.vulkan_inner().image().expect("image");
        unsafe {
            device
                .upload_buffer_to_image(staging.buffer(), image, w, h)
                .expect("upload");
        }
    }

    /// Read one pixel from a texture via the RHI's readback primitive.
    fn read_pixel(
        device: &Arc<HostVulkanDevice>,
        texture: &Texture,
        x: u32,
        y: u32,
    ) -> (u8, u8, u8, u8) {
        let w = texture.width();
        let h = texture.height();
        let readback = VulkanTextureReadback::new(
            device,
            &TextureReadbackDescriptor {
                label: "blending-test-readback",
                format: TextureFormat::Bgra8Unorm,
                width: w,
                height: h,
            },
        )
        .expect("readback");
        let ticket = readback
            .submit(texture, TextureSourceLayout::ShaderReadOnly)
            .expect("readback submit");
        let mut sample: (u8, u8, u8, u8) = (0, 0, 0, 0);
        readback
            .wait_and_read_with(ticket, u64::MAX, |bgra| -> std::io::Result<()> {
                let idx = ((y * w + x) * 4) as usize;
                sample = (bgra[idx], bgra[idx + 1], bgra[idx + 2], bgra[idx + 3]);
                Ok(())
            })
            .expect("readback wait")
            .expect("readback read closure");
        sample
    }

    #[test]
    fn new_compiles_kernel() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let result = SandboxedBlendingCompositor::new(&device);
        assert!(
            result.is_ok(),
            "compositor creation must succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn output_matches_video_when_only_video_bound() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let compositor = SandboxedBlendingCompositor::new(&device).expect("compositor");

        let video = make_render_texture(&device, 64, 32);
        let output = make_render_texture(&device, 64, 32);
        // BGRA = (10, 200, 50, 255) → opaque green-ish.
        fill_texture_solid(&device, &video, 10, 200, 50, 255);

        compositor
            .dispatch(BlendingCompositorInputs {
                video: Some(BlendingLayer {
                    texture: &video,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                lower_third: None,
                watermark: None,
                pip: None,
                output: BlendingOutput { texture: &output },
                pip_slide_progress: 0.0,
            })
            .expect("dispatch");

        // ±1 tolerance per channel for unorm round-trip.
        let (b, g, r, a) = read_pixel(&device, &output, 16, 16);
        assert!((b as i32 - 10).abs() <= 1, "B={b}");
        assert!((g as i32 - 200).abs() <= 1, "G={g}");
        assert!((r as i32 - 50).abs() <= 1, "R={r}");
        assert!((a as i32 - 255).abs() <= 1, "A={a}");
    }

    #[test]
    fn no_video_falls_back_to_dark_blue() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let compositor = SandboxedBlendingCompositor::new(&device).expect("compositor");
        let output = make_render_texture(&device, 32, 32);

        compositor
            .dispatch(BlendingCompositorInputs {
                video: None,
                lower_third: None,
                watermark: None,
                pip: None,
                output: BlendingOutput { texture: &output },
                pip_slide_progress: 0.0,
            })
            .expect("dispatch");

        // Fragment shader's no-video fallback is vec4(0.05, 0.05, 0.12, 1.0)
        // → BGRA roughly (31, 13, 13, 255).
        let (b, g, r, a) = read_pixel(&device, &output, 8, 8);
        let expected_b = (0.12_f32 * 255.0).round() as i32; // 31
        let expected_g = (0.05_f32 * 255.0).round() as i32; // 13
        let expected_r = (0.05_f32 * 255.0).round() as i32; // 13
        assert!((b as i32 - expected_b).abs() <= 1, "B={b}");
        assert!((g as i32 - expected_g).abs() <= 1, "G={g}");
        assert!((r as i32 - expected_r).abs() <= 1, "R={r}");
        assert_eq!(a, 255, "alpha must be opaque on fallback");
    }

    #[test]
    fn rejects_layer_size_mismatch() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let compositor = SandboxedBlendingCompositor::new(&device).expect("compositor");

        let video = make_render_texture(&device, 32, 32);
        let output = make_render_texture(&device, 64, 32);
        fill_texture_solid(&device, &video, 0, 0, 0, 255);

        let err = compositor
            .dispatch(BlendingCompositorInputs {
                video: Some(BlendingLayer {
                    texture: &video,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                lower_third: None,
                watermark: None,
                pip: None,
                output: BlendingOutput { texture: &output },
                pip_slide_progress: 0.0,
            })
            .expect_err("size mismatch must error");
        assert!(matches!(err, Error::GpuError(_)));
    }

    /// Multi-layer composite smoke — exercises the full alpha-over
    /// path with all 4 inputs bound. Asserts every layer composite path
    /// executes without error and the PiP frame chrome lands in the
    /// upper-right when `pip_slide_progress = 1.0`.
    #[test]
    fn multi_layer_composite_writes_each_layer() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let compositor = SandboxedBlendingCompositor::new(&device).expect("compositor");

        let w: u32 = 320;
        let h: u32 = 240;
        let pip_w: u32 = 96;
        let pip_h: u32 = 64;

        let video = make_render_texture(&device, w, h);
        let lower_third = make_render_texture(&device, w, h);
        let watermark = make_render_texture(&device, w, h);
        let pip = make_render_texture(&device, pip_w, pip_h);
        let output = make_render_texture(&device, w, h);

        fill_texture_solid(&device, &video, 128, 128, 128, 255);
        fill_texture_solid(&device, &watermark, 0, 0, 0, 0);
        fill_texture_solid(&device, &lower_third, 0, 0, 0, 0);
        fill_texture_solid(&device, &pip, 255, 255, 0, 255);

        compositor
            .dispatch(BlendingCompositorInputs {
                video: Some(BlendingLayer {
                    texture: &video,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                lower_third: Some(BlendingLayer {
                    texture: &lower_third,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                watermark: Some(BlendingLayer {
                    texture: &watermark,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                pip: Some(BlendingLayer {
                    texture: &pip,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                output: BlendingOutput { texture: &output },
                pip_slide_progress: 1.0,
            })
            .expect("dispatch with 4 layers must succeed");

        let (b, g, r, a) = read_pixel(&device, &output, w / 2, h / 2);
        assert!((b as i32 - 128).abs() <= 2, "B={b}");
        assert!((g as i32 - 128).abs() <= 2, "G={g}");
        assert!((r as i32 - 128).abs() <= 2, "R={r}");
        assert_eq!(a, 255, "A={a}");

        // Pixel inside the PiP content rect (PiP docks right edge at
        // pip_slide_progress=1.0). Hardware-bilinear sample of opaque
        // cyan; tolerance accounts for chroma drift at the rect edge.
        let pip_sample_x = ((1.0 - 0.02 - 0.28 * 0.5) * (w as f32)) as u32;
        let pip_sample_y = ((0.02 + 0.35 * 0.5) * (h as f32)) as u32;
        let (b2, g2, r2, _a2) = read_pixel(&device, &output, pip_sample_x, pip_sample_y);
        assert!(
            b2 > 200 && g2 > 200 && r2 < 30,
            "PiP content sample expected cyan-dominant, got BGRA=({b2}, {g2}, {r2}, _)"
        );
    }
}
