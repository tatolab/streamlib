// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 80s Blade Runner CRT + film-grain post-effect kernel — sandboxed
//! scenario content.
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
//! - [`VulkanGraphicsKernel::offscreen_render`] — the cdylib-safe
//!   Texture-typed render-scope helper. Opens the dynamic-rendering
//!   pass internally, transitions output `UNDEFINED →
//!   COLOR_ATTACHMENT_OPTIMAL`, records `cmd_bind_and_draw`, submits
//!   through the host's queue mutex, waits. Output is left in
//!   `COLOR_ATTACHMENT_OPTIMAL`.
//! - [`RhiCommandRecorder`] + [`record_image_barrier`] — for the
//!   input-side transition (when the input isn't already
//!   `SHADER_READ_ONLY_OPTIMAL`) and the post-pass
//!   `COLOR_ATTACHMENT_OPTIMAL → SHADER_READ_ONLY_OPTIMAL` transition
//!   on the output.
//!
//! Neither surface exposes raw `vulkanalia` types — the kernel is
//! cdylib-safe end-to-end. Every queue-mutex / fence / Drop / barrier
//! bug the engine has fixed propagates here for free.
//!
//! ## Lifecycle
//!
//! Caller pre-allocates a ring of output `Texture`s (mirrors
//! `BlendingCompositor`'s `OUTPUT_RING_DEPTH = 2`), hands one to
//! [`SandboxedCrtFilmGrain::dispatch`] per frame along with the input
//! texture + its current Vulkan layout, and `dispatch` returns once
//! the GPU has signaled the underlying submits. After return, both
//! input and output textures are in `SHADER_READ_ONLY_OPTIMAL`, ready
//! for the next consumer to sample without re-barriering.
//!
//! [`record_image_barrier`]: streamlib::sdk::engine::host_rhi::RhiCommandRecorder::record_image_barrier

use std::sync::{Arc, Mutex};

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
    TextureFormat,
    VertexInputState,
    Viewport,
    VulkanLayout,
};
use streamlib::sdk::error::{Result, Error};
use streamlib::sdk::engine::host_rhi::{
    HostVulkanDevice,
    OffscreenColorTarget,
    OffscreenDraw,
    RhiCommandRecorder,
    VulkanAccess,
    VulkanGraphicsKernel,
    VulkanStage,
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
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>) -> Result<Self> {
        let label = "crt_film_grain";

        let vert =
            include_bytes!(concat!(env!("OUT_DIR"), "/crt_film_grain.vert.spv"));
        let frag =
            include_bytes!(concat!(env!("OUT_DIR"), "/crt_film_grain.frag.spv"));

        let stages = [GraphicsStage::vertex(vert), GraphicsStage::fragment(frag)];
        let bindings = [
            GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
        ];
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
        let kernel = VulkanGraphicsKernel::new(vulkan_device, &descriptor)?;

        let recorder = RhiCommandRecorder::new(vulkan_device, "crt_film_grain_recorder")?;

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

        if inputs.input.texture.width() != width
            || inputs.input.texture.height() != height
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::engine::HostTextureExt;
    use streamlib::sdk::rhi::{
        TextureDescriptor,
        TextureReadbackDescriptor,
        TextureSourceLayout,
        TextureUsages,
    };
    use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, VulkanTextureReadback};

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

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

    /// Read a single BGRA pixel via the RHI readback primitive.
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
                label: "crt-test-readback",
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

    /// Read the full BGRA buffer from a texture for visual-smoke tests.
    fn read_bgra_buffer(
        device: &Arc<HostVulkanDevice>,
        texture: &Texture,
    ) -> Vec<u8> {
        let w = texture.width();
        let h = texture.height();
        let readback = VulkanTextureReadback::new(
            device,
            &TextureReadbackDescriptor {
                label: "crt-test-readback-full",
                format: TextureFormat::Bgra8Unorm,
                width: w,
                height: h,
            },
        )
        .expect("readback");
        let ticket = readback
            .submit(texture, TextureSourceLayout::ShaderReadOnly)
            .expect("readback submit");
        let mut bytes = vec![0u8; (w * h * 4) as usize];
        readback
            .wait_and_read_with(ticket, u64::MAX, |bgra| -> std::io::Result<()> {
                bytes.copy_from_slice(bgra);
                Ok(())
            })
            .expect("readback wait")
            .expect("readback read closure");
        bytes
    }

    fn default_inputs<'a>(
        input: &'a Texture,
        output: &'a Texture,
    ) -> CrtFilmGrainInputs<'a> {
        CrtFilmGrainInputs {
            input: CrtFilmGrainInput {
                texture: input,
                current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            },
            output: CrtFilmGrainOutput { texture: output },
            time_seconds: 0.0,
            crt_curve: 0.7,
            scanline_intensity: 0.6,
            chromatic_aberration: 0.004,
            grain_intensity: 0.18,
            grain_speed: 1.0,
            vignette_intensity: 0.5,
            brightness: 2.2,
        }
    }

    #[test]
    fn new_compiles_kernel() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let result = SandboxedCrtFilmGrain::new(&device);
        assert!(
            result.is_ok(),
            "kernel creation must succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn rejects_size_mismatch() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let kernel = SandboxedCrtFilmGrain::new(&device).expect("kernel");
        let input = make_render_texture(&device, 32, 32);
        let output = make_render_texture(&device, 64, 32);
        fill_texture_solid(&device, &input, 0, 0, 0, 255);

        let err = kernel
            .dispatch(default_inputs(&input, &output))
            .expect_err("size mismatch must error");
        assert!(matches!(err, Error::GpuError(_)));
    }

    /// Runs the kernel against a uniform mid-grey input. The center of
    /// the output (well inside the barrel-distorted screen rect) must
    /// be non-black after CRT processing, and the far corners must be
    /// black (outside the curved bounds → explicitly zeroed by the
    /// shader's outside-bounds carve-out).
    #[test]
    fn solid_input_produces_curved_bounds() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let kernel = SandboxedCrtFilmGrain::new(&device).expect("kernel");

        let w: u32 = 256;
        let h: u32 = 192;
        let input = make_render_texture(&device, w, h);
        let output = make_render_texture(&device, w, h);
        // Mid-grey opaque BGRA = (128, 128, 128, 255).
        fill_texture_solid(&device, &input, 128, 128, 128, 255);

        let mut inputs = default_inputs(&input, &output);
        // Disable grain so we can make deterministic assertions about
        // the center pixel; barrel curve stays in to test the bounds
        // carve-out.
        inputs.grain_intensity = 0.0;
        kernel.dispatch(inputs).expect("dispatch");

        let (cb, cg, cr, _ca) = read_pixel(&device, &output, w / 2, h / 2);
        assert!(
            cb as u32 + cg as u32 + cr as u32 > 0,
            "center pixel must not be fully black after CRT pass: BGR=({cb},{cg},{cr})"
        );

        let (b0, g0, r0, _a0) = read_pixel(&device, &output, 0, 0);
        assert_eq!(
            (b0, g0, r0),
            (0, 0, 0),
            "top-left corner must be zeroed by outside-bounds carve-out"
        );
    }

    /// Visual smoke: feeds a checkerboard + magenta block + green
    /// diagonal through the kernel, dispatches, writes a PNG of the
    /// result for human review. Mirrors the engine version's shape.
    #[test]
    fn visual_smoke_emits_png() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let kernel = SandboxedCrtFilmGrain::new(&device).expect("kernel");

        let w: u32 = 480;
        let h: u32 = 320;
        let input = make_render_texture(&device, w, h);
        let output = make_render_texture(&device, w, h);

        // Compose a synthetic input via staging buffer: 32x32
        // checkerboard with a magenta block in the upper-left and a
        // green diagonal stripe.
        let staging =
            HostVulkanBuffer::new(&device, (w as u64) * (h as u64) * (4 as u64)).expect("staging");
        unsafe {
            let ptr = staging.mapped_ptr() as *mut u32;
            for y in 0..h {
                for x in 0..w {
                    let cell = ((x / 32) + (y / 32)) % 2 == 0;
                    let mut b = if cell { 200u32 } else { 60u32 };
                    let mut g = if cell { 200u32 } else { 60u32 };
                    let mut r = if cell { 200u32 } else { 60u32 };
                    if x < 96 && y < 96 {
                        b = 255;
                        g = 0;
                        r = 255;
                    }
                    let on_diag = (x as i32 - y as i32).abs() < 8;
                    if on_diag {
                        b = 0;
                        g = 240;
                        r = 0;
                    }
                    let pixel = b | (g << 8) | (r << 16) | (255u32 << 24);
                    *ptr.add((y * w + x) as usize) = pixel;
                }
            }
        }
        let image = input.vulkan_inner().image().expect("image");
        unsafe {
            device
                .upload_buffer_to_image(staging.buffer(), image, w, h)
                .expect("upload");
        }

        let mut inputs = default_inputs(&input, &output);
        // Pick a non-zero animation phase so scanlines + grain are
        // visible in the rendered PNG.
        inputs.time_seconds = 0.4;
        kernel.dispatch(inputs).expect("dispatch must succeed");

        let bgra_bytes = read_bgra_buffer(&device, &output);
        let bgra_size = bgra_bytes.len();
        let mut rgba = vec![0u8; bgra_size];
        for chunk in 0..(bgra_size / 4) {
            let i = chunk * 4;
            rgba[i] = bgra_bytes[i + 2];
            rgba[i + 1] = bgra_bytes[i + 1];
            rgba[i + 2] = bgra_bytes[i];
            rgba[i + 3] = bgra_bytes[i + 3];
        }

        let out_path = std::env::var("STREAMLIB_CRT_FILM_GRAIN_PNG_OUT")
            .unwrap_or_else(|_| "target/crt_film_grain_smoke.png".to_string());
        let _ = std::fs::create_dir_all(
            std::path::Path::new(&out_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );
        let file = std::fs::File::create(&out_path)
            .unwrap_or_else(|e| panic!("create {out_path}: {e}"));
        let bw = std::io::BufWriter::new(file);
        let mut encoder = png::Encoder::new(bw, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&rgba).expect("PNG data");
        eprintln!("crt_film_grain visual smoke wrote {out_path}");
    }
}
